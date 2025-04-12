use openssh::Stdio;
use tracing::error;

use crate::hosts::Host;

impl Host {
    /// Connect to a wireless network, optionally with a password.
    pub async fn associate(&self, ssid: &str, password: Option<&str>) -> anyhow::Result<()> {
        let mut command = self.session.command("sudo");
        command.args(["nmcli", "device", "wifi", "connect", ssid]);

        if let Some(password) = password {
            command.args(["password", password]);
        }

        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let out = command.output().await?;
        if !out.status.success() {
            error!(host = self.id, "failed to connect to Wi-Fi network");
            anyhow::bail!(
                "connecting to Wi-Fi network exited with error code {}",
                out.status
            );
        }
        Ok(())
    }
}
