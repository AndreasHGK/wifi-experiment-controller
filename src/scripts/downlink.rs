use std::{path::Path, time::Duration};

use anyhow::Context;
use clap::Parser;
use tokio::{fs::File, io::AsyncWriteExt, select, time::sleep};
use tracing::{debug, error, info, warn};

use crate::{hosts::Hosts, monitor::MonitorConfig, utils::run_all};

#[derive(Parser, Debug, Clone)]
pub struct DowlinkArgs {
    /// The host id of the wireless access point.
    #[clap(long = "ap")]
    pub access_point: String,
    /// The host id(s) of the hosts that will capture the wireless traffic.
    #[clap(long, required = true)]
    pub monitors: Vec<String>,
    /// How the capture should last in seconds.
    #[clap(short = 'd', long, default_value = "10")]
    pub duration: u64,
    /// Whether to use UDP.
    #[clap(
        short = 'U',
        long = "udp",
        required = true,
        requires_if("true", "total_bandwidth")
    )]
    pub udp: Option<bool>,
    /// The total bandwidth that the clients should use together.
    ///
    /// This will be divided equally over each client. Use 0 for unlimited bandwidth.
    #[clap(short = 'B', long = "bandwidth", default_value = "0")]
    pub total_bandwidth: u64,
    /// Configure the MCS.
    ///
    /// Follows the format of `iw dev <if> set bitrates <mcs...>`. For example: `he-mcs-5 1:11`.
    /// Leave empty to use automatic MCS.
    #[clap(long)]
    pub mcs: Option<String>,
}

pub async fn run(args: DowlinkArgs, hosts: Hosts, out_path: &Path) -> anyhow::Result<()> {
    let total_bandwidth = args.total_bandwidth;
    let udp = args.udp.unwrap_or(true);

    let senders: Vec<_> = hosts
        .iter()
        .filter(|host| !(&args.access_point == &host.id || args.monitors.contains(&host.id)))
        .collect();

    let access_point = hosts
        .get(args.access_point)
        .context("access point id not found")?
        .clone();

    let Some(access_point_ip) = access_point.extra_data.interface.clone() else {
        anyhow::bail!("Access point should have a wireless interface IP configured");
    };

    tokio::fs::create_dir_all(&out_path)
        .await
        .expect("could not create output folder");

    // Configure the MCS on the access point.
    let output = access_point
        .session
        .shell(format!(
            "iw dev phy1-ap0 set bitrates {}",
            args.mcs.as_ref().map(|v| v.as_str()).unwrap_or("")
        ))
        .output()
        .await
        .context("failed to set MCS")?;

    if !output.status.success() {
        debug!(
            stdout = %String::from_utf8_lossy(&output.stdout),
            stderr = %String::from_utf8_lossy(&output.stderr),
            "Failed to set MCS"
        );
        anyhow::bail!("setting MCS exited with error code {}", output.status);
    }

    // Configure and start the monitoring.
    let monitor = MonitorConfig {
        ssid: "OpenWrt".to_string(),
        bssid: "10:7c:61:df:7a:d2".to_string(),
        monitors: args.monitors.clone(),
        targets: senders.iter().map(|v| v.id.clone()).collect(),
        // Give some extra leeway to ensure the monitor captures everything.
        duration: Duration::from_secs(args.duration + 4),
        output_path: Some(out_path.to_owned()),
        // TODO: how can this be automated in OpenWRT?
        frequency: 5580,
        bandwidth: 80,
        set_aids: true,
    }
    .start(&hosts)
    .await
    .context("failed to start capture")?;

    let mut start_port = 5000;
    let iperf_client_num = senders.len();

    // Start the iperf servers on the access point.
    let access_point_ip2 = access_point_ip.clone();
    let aps = tokio::spawn(async move {
        info!("Starting iperf servers");
        let mut n = start_port;
        run_all(vec![&access_point; iperf_client_num], |_| {
            n += 1;
            format!("iperf3 -s {access_point_ip2} -p {n} -1")
        })
        .await
        .unwrap();
    });

    // Ensure all iperf servers have been started before starting the clients. This is slightly
    // hackty but the simplest way.
    sleep(Duration::from_secs(1)).await;

    // Run iperf clients on each NUC.
    info!("Starting iperf clients");
    let mut ip_num = 0;
    let iperfs = run_all(senders.clone(), |h| {
        if h.extra_data.interface.is_none() {
            warn!(
                host = h.id,
                "Host does not have an interface set in the hosts file"
            );
        }

        start_port += 1;
        let s = format!(
            "iperf3 -c {access_point_ip} -p {start_port} {0} -R -b {1} {2}",
            // 0 - Bind address
            h.extra_data
                .interface
                .as_ref()
                .map(|ip| format!("-B {ip}"))
                .unwrap_or_else(|| "".to_string()),
            // 1 - Bandwidth
            total_bandwidth / senders.len() as u64,
            // 2 - Use UDP or not
            if udp { "-u" } else { "" }
        );
        ip_num += 1;
        s
    })
    .await
    .unwrap();

    // Write all the iperf outputs to files.
    for (host, iperf) in iperfs.into_iter() {
        if !iperf.status.success() {
            error!(host = host.id, "Iperf failed");
        }

        let mut f = File::create_new(out_path.join(&format!("{}.txt", host.id)))
            .await
            .unwrap();
        f.write_all(&iperf.stdout).await.unwrap();

        // Also write error output if it exists.
        if !iperf.stderr.is_empty() {
            let mut f = File::create_new(out_path.join(&format!("{}.stderr.txt", host.id)))
                .await
                .unwrap();
            f.write_all(&iperf.stderr).await.unwrap();
        }
    }

    info!("Waiting for capture to finish");
    monitor.wait().await.unwrap();

    debug!("Waiting for AP to finish");
    select! {
        _ = tokio::time::sleep(Duration::from_secs(1)) => {
            // Close the remaining iperf sessions.
            _ = hosts.get("ap").unwrap().session.shell("killall iperf3").output().await;

            anyhow::bail!("AP iperf servers did not close correctly; remaining sessions killed");
        },
        result = aps => {
            _ = result.context("iperf on AP failed")?;
        },
    }

    Ok(())
}
