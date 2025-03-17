use anyhow::Context;
use openssh::Stdio;
use tracing::debug;

use crate::hosts::{Host, HostOs};

#[derive(Debug, Clone, Copy)]
pub enum Package {
    Wireshark,
}

impl Package {
    /// Get the name of the package in a specific OS's package manager.
    pub fn to_os_package(&self, os: &HostOs) -> Option<&'static str> {
        if os.is_other() {
            return None;
        }

        let pkg = match self {
            Package::Wireshark => "wireshark",
        };
        Some(pkg)
    }
}

impl Host {
    /// Installs a package on a system if it is not yet installed, making it abailable to be used in
    /// the PATH.
    pub async fn install_package(&self, pkg: Package) -> anyhow::Result<&Self> {
        let Some(pkg_name) = pkg.to_os_package(&self.os_info) else {
            anyhow::bail!("package is not available for host's os: {pkg:?}");
        };

        match self.os_info {
            HostOs::Ubuntu => {
                let session = &self.session;
                let output = session
                    .command("sudo")
                    .arg("apt-get")
                    .arg("--quiet")
                    .arg("install")
                    .arg(pkg_name)
                    .arg("-y")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .output()
                    .await
                    .context("package installation failed")?;

                debug!(host = self.id, os = %self.os_info, "Package installation output: {:?}", output);
                Ok(self)
            }
            HostOs::NixOS => anyhow::bail!("trying to install packages on unsupported OS"),
            HostOs::Other(_) => anyhow::bail!("trying to install packages on unsupported OS"),
        }
    }
}
