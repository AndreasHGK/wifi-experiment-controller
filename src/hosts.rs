use std::{
    collections::{HashMap, HashSet},
    fmt::Display,
    path::Path,
    sync::Arc,
};

use anyhow::Context;
use openssh::{KnownHosts, SessionBuilder};
use serde::Deserialize;
use tokio::{fs, task::JoinSet};
use tracing::{debug, info};

/// A configuration object containing information about all the hosts that should be used in the
/// setup.
#[derive(Debug, Deserialize, Clone)]
pub struct HostsConfig {
    /// A list of hosts and their configuration.
    #[serde(rename = "host")]
    pub hosts: Vec<HostConfig>,
}

/// Configuration for a single host.
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct HostConfig {
    /// Identifier for the host in experiments. Should be unique across hosts.
    ///
    /// Can be different from the hostname of the system.
    pub id: HostId,
    /// The SSH url to use to connect to the host.
    ///
    /// If relays are set, this needs to be the url accessible from the last relay set.
    pub url: String,
    /// Relay SSH host(s) to jump through to connect to the host. The first entry is the first relay
    /// that will be connected to.
    #[serde(default)]
    pub relays: Vec<String>,
    /// Extra fields included in hosts.
    #[serde(flatten)]
    pub extra_data: ExtraData,
}

/// Extra data used in scripts.
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "kebab-case")]
pub struct ExtraData {
    /// The wireless driver used for the Wi-Fi interface in the device.
    pub wifi_driver: Option<String>,
    /// The IP address of the main wireless interface on this machine.
    pub interface: Option<String>,
}

impl HostsConfig {
    /// Reads a hosts configuration file to a [HostsConfig] object.
    pub async fn read(p: impl AsRef<Path>) -> anyhow::Result<Self> {
        let conf = fs::read_to_string(p).await?;
        let hosts: Self = toml::from_str(&conf)?;
        hosts.validate()?;
        Ok(hosts)
    }

    /// Validate the configuration for any disallowed values.
    fn validate(&self) -> anyhow::Result<()> {
        // Ensures there are no duplicate ids.
        let mut ids = HashSet::with_capacity(self.hosts.len());
        for host in &self.hosts {
            if ids.insert(host.id.as_str()) == false {
                return Err(anyhow::Error::msg(format!(
                    "duplicate host id: `{}`",
                    host.id
                )));
            }
        }

        Ok(())
    }

    /// Connects to all the hosts specified in the configuration. Returns an error if not all hosts
    /// could be connected to.
    pub async fn connect(&self) -> anyhow::Result<Hosts> {
        // The config should be valid. This was also ran if the config has been read from a file,
        // but it does not hurt to validate it twice.
        self.validate().context("configuration is not valid")?;

        let mut hosts = HashMap::with_capacity(self.hosts.len());

        // Concurrently connect to all hosts and get the necessary info.
        let mut tasks = JoinSet::new();
        for host in &self.hosts {
            let host = host.clone();

            tasks.spawn(async move { host.connect().await });
        }

        // Wait for all connections to be completed. If any of the connections fail, return with an
        // error. All other connections will be aborted.
        while let Some(next_host) = tasks.join_next().await {
            let host = next_host??;
            let id = host.id.clone();
            info!(id, os = %host.os_info, "Successfully connected to host");

            if hosts.insert(host.id.clone(), Arc::new(host)).is_some() {
                // SAFETY: The config was validated at the beginning of the function.
                unreachable!("Duplicate host id `{}`", id);
            }
        }

        Ok(Hosts { map: hosts })
    }
}

impl HostConfig {
    /// Try to connect to the host with the provided configuration.
    async fn connect(&self) -> anyhow::Result<Host> {
        let mut builder = SessionBuilder::default();
        builder.known_hosts_check(KnownHosts::Accept);
        builder.jump_hosts(self.relays.iter());

        let session = builder
            .connect(&self.url)
            .await
            .context(format!("error while opening session to `{}`", &self.id))?;
        debug!(id = &self.id, "Opened ssh session");

        // Get info about the OS of the remote machine.
        let os_info = session
            .command("cat")
            .raw_arg("/etc/*-release")
            .output()
            .await?;
        let os_info = String::from_utf8_lossy(&os_info.stdout);

        // Parse the OS info. We're looking for the following pattern: `DISTRIB_ID=id`.
        let os_id = os_info
            .split('\n')
            .filter_map(|line| line.split_once('='))
            .find(|(k, _)| k.eq_ignore_ascii_case("DISTRIB_ID"))
            .map(|(_, v)| v);

        let os_info = match os_id {
            Some(other) => HostOs::from_distrib_id(other),
            None => HostOs::Other(String::new()),
        };
        debug!(id = self.id, "Detected OS: {os_info}");

        Ok(Host {
            id: self.id.clone(),
            session,
            os_info,
            extra_data: self.extra_data.clone(),
        })
    }
}

/// Uniquely identifies a host in the setup.
pub type HostId = String;

#[derive(Debug)]
pub struct Hosts {
    map: HashMap<HostId, Arc<Host>>,
}

impl Hosts {
    /// Get an iterator over hosts based on the specified identifiers.
    ///
    /// If identifiers are not found this function returns an error with the first identifier that
    /// could not be found.
    pub fn get_many<I, A>(&self, ids: I) -> Result<impl Iterator<Item = &Arc<Host>> + Clone, A>
    where
        I: IntoIterator<Item = A>,
        I::IntoIter: Clone,
        A: AsRef<str>,
    {
        let iter = ids.into_iter();

        // Make sure all host IDs were able to be found.
        if let Some(missing) = iter.clone().find(|id| self.get(id.as_ref()).is_none()) {
            return Err(missing);
        }

        // SAFETY: We checked before if all IDs map to a host.
        Ok(iter.map(|id| self.map.get(id.as_ref()).expect("host should exist")))
    }

    /// Return an iterator over all hosts except those specified in `excluded_ids`.
    ///
    /// Unknown IDs are ignored.
    pub fn all_except<I, A>(&self, excluded_ids: I) -> impl Iterator<Item = &Arc<Host>> + Clone
    where
        I: IntoIterator<Item = A>,
        A: AsRef<str>,
    {
        let set: HashSet<_> = excluded_ids
            .into_iter()
            .filter_map(|id| self.get(id))
            .map(|host| &host.id)
            .collect();

        self.map
            .values()
            .filter(move |host| !set.contains(&host.id))
    }

    /// Get a host by its identifier.
    pub fn get(&self, id: impl AsRef<str>) -> Option<&Arc<Host>> {
        self.map.get(id.as_ref())
    }

    /// Iterate over all hosts.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<Host>> {
        self.map.iter().map(|(_, v)| v)
    }
}

/// A remote host on which commands can be ran.
#[derive(Debug)]
pub struct Host {
    /// A unique identifier for the host.
    pub id: HostId,
    /// An SSH session to the remote host.
    pub session: openssh::Session,
    pub os_info: HostOs,
    pub extra_data: ExtraData,
}

/// Information about the host's operating system. Can be useful to known for instance which package
/// manager is available.
#[derive(Debug, Clone)]
pub enum HostOs {
    NixOS,
    Ubuntu,
    Other(String),
}

impl HostOs {
    /// Returns true if the OS is not one of the known operating systems.
    pub fn is_other(&self) -> bool {
        if let Self::Other(_) = self {
            return true;
        }
        false
    }

    fn from_distrib_id(id: impl AsRef<str>) -> Self {
        match id.as_ref() {
            "nixos" => HostOs::NixOS,
            "Ubuntu" => HostOs::Ubuntu,
            other => HostOs::Other(other.to_string()),
        }
    }
}

impl Display for HostOs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostOs::NixOS => f.write_str("NixOS"),
            HostOs::Ubuntu => f.write_str("Ubuntu"),
            HostOs::Other(name) => {
                f.write_str("Other OS")?;
                if !name.is_empty() {
                    f.write_str(" (")?;
                    f.write_str(name)?;
                    f.write_str(")")?;
                }
                Ok(())
            }
        }
    }
}
