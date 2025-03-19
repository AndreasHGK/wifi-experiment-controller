//! Utilities for systems with the `iwlwifi` driver.

use anyhow::Context;

use crate::hosts::Host;

/// Change the association ID of the wireless interface for monitoring.
///
/// * `aid` - The association ID to monitor.
/// * `bssid` - The BSSID as a string representing a mac address.
pub async fn set_association_id(host: &Host, aid: u16, bssid: &str) -> anyhow::Result<()> {
    let status = host
        .session
        .command("sudo")
        .arg("sh")
        .arg("-c")
        .arg(format!(
            // The AID needs to be a hexidecimal number.
            "echo {aid:x} {bssid} > /sys/kernel/debug/iwlwifi/*/iwlmvm/he_sniffer_params"
        ))
        .status()
        .await
        .context("failed to change AID")?;

    if !status.success() {
        anyhow::bail!("changing AID exited with status code {status}");
    }

    Ok(())
}
