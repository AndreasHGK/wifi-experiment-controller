use std::{path::PathBuf, time::Duration};

use anyhow::Context;
use openssh::Stdio;
use tokio::{fs, io::AsyncReadExt, task::JoinSet};
use tracing::{debug, info};

use crate::{
    capture::{Capture, CaptureConfig, StopCondition},
    driver::wifi::iwlwifi,
    hosts::{HostId, Hosts},
};

pub struct MonitorConfig {
    /// The SSID of the network to monitor.
    pub ssid: String,
    /// Thee BSS ID of the network to monitor.
    pub bssid: String,
    /// The hosts that will perform the monitoring.
    pub monitors: Vec<HostId>,
    /// The hosts to monitor.
    pub targets: Vec<HostId>,
    pub duration: Duration,
    /// Where to write the captures to.
    pub output_path: Option<PathBuf>,
    /// If true, gathers the association IDs of all the other hosts and assign each one to a
    /// different monitor device.
    ///
    /// This requires that the monitor driver supports manually setting an association ID.
    pub set_aids: bool,
}

impl MonitorConfig {
    /// Start monitoring traffic.
    pub async fn start(self: Self, hosts: &Hosts) -> anyhow::Result<Monitor> {
        if let Some(output_path) = &self.output_path {
            fs::create_dir_all(output_path)
                .await
                .context("could not create output path")?;
        }

        let monitor_hosts = hosts
            .get_many(self.monitors.iter().map(|v| v.as_str()))?
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();

        // Connect the target hosts and determine their association ID.
        let connected_hosts = hosts
            .get_many(self.targets.iter().map(|v| v.as_str()))?
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();

        if self.set_aids {
            let h = monitor_hosts
                .get(0)
                .context("monitoring requires at least one monitor host")?;
            debug!(host = h.id, "Listening for AIDs");

            // Set up the actual capture that will find the association ids.
            let mut aid_capture = h
                .session
                .command("sudo")
                .args([
                    "tshark",
                    "-T",
                    "fields",
                    "--interface",
                    "mon0",
                    // Return only the association ID.
                    "-e",
                    "wlan.fixed.aid",
                    // Filter out all packets that arent "association response" or packets in a
                    // different BSS.
                    "-Y",
                    &format!(
                        "wlan.fc.type_subtype == 0x0001 && wlan.bssid == {:?}",
                        self.bssid
                    ),
                ])
                .stderr(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .await
                .context("failed to start AID monitor capture")?;

            // Connect all the non monitor hosts to the AP so the monitor can find their AID.
            let mut connection_join_set = JoinSet::new();
            for connected_host in connected_hosts {
                let ssid = self.ssid.clone();
                connection_join_set
                    .spawn(async move { connected_host.associate(&ssid, None).await });
            }
            // Ensure all the nodes have successfully associated to the network.
            for result in connection_join_set.join_all().await {
                result?;
            }

            let mut aids = String::new();
            aid_capture
                .stdout()
                .as_mut()
                .expect("stdout was previously set to Stdio::piped()")
                .read_to_string(&mut aids)
                .await
                .context("failed to read AID capture output to string")?;

            let aids = aids
                .lines()
                .skip(1)
                .map(|v| v.strip_prefix("0x").unwrap_or(v))
                .map(|v| u16::from_str_radix(v, 16))
                .try_fold(Vec::new(), |mut acc, next| {
                    acc.push(next?);
                    anyhow::Result::<_>::Ok(acc)
                })
                .context("could not parse association ID")?;

            // if aids.len() < self.targets.len() {
            //     anyhow::bail!("expected {} aids, got {}", self.targets.len(), aids.len());
            // }
            let aids = vec![1];

            for (aid, host) in aids.iter().zip(monitor_hosts.iter()) {
                debug!(
                    host = host.id,
                    aid, "Changing association ID on monitor host"
                );
                match host.wifi_driver.as_ref().map(|s| s.as_str()) {
                    Some("iwlwifi") => iwlwifi::set_association_id(&host, *aid, &self.bssid)
                        .await
                        .context("failed to set AID")?,
                    other => {
                        anyhow::bail!(
                            "cannot set association ID for unsupported driver ({}) on host {}",
                            other.unwrap_or("unknown"),
                            host.id,
                        );
                    }
                }
            }
        }

        // Start the capture on all the monitor hosts.
        let mut captures = JoinSet::new();
        info!(
            "Starting monitor with {} monitor hosts",
            monitor_hosts.len()
        );
        for monitor_host in monitor_hosts {
            let output_path = self.output_path.clone();
            captures.spawn(async move {
                monitor_host
                    .capture(&CaptureConfig {
                        interface: "mon0".to_string(),
                        stop_condition: StopCondition::Duration(self.duration),
                        output_path: output_path
                            .map(|v| v.join(&monitor_host.id).with_extension("pcapng")),
                    })
                    .await
                    .map(|res| (monitor_host.id.clone(), res))
            });
        }
        Ok(Monitor { captures })
    }
}

pub struct Monitor {
    captures: JoinSet<anyhow::Result<(HostId, Capture)>>,
}

impl Monitor {
    /// Waits for all the captures to complete and returns their results.
    pub async fn wait(self: Self) -> anyhow::Result<Vec<(HostId, Capture)>> {
        let result =
            self.captures
                .join_all()
                .await
                .into_iter()
                .try_fold(Vec::new(), |mut acc, item| {
                    acc.push(item.context("capture returned an error")?);
                    anyhow::Result::<_>::Ok(acc)
                })?;

        info!("Monitor complete");
        Ok(result)
    }

    /// Immediately stops the captures, throwing away the results.
    pub fn abort(self: &mut Self) {
        self.captures.abort_all();
    }
}
