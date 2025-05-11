use std::{path::Path, time::Duration};

use anyhow::{anyhow, Context};
use clap::{Parser, ValueEnum};
use ron::ser::{to_string_pretty, PrettyConfig};
use serde::Serialize;
use tokio::{fs::File, io::AsyncWriteExt, select, time::sleep};
use tracing::{debug, error, info, warn};

use crate::{hosts::Hosts, monitor::MonitorConfig, utils::run_all};

#[derive(Parser, Debug, Clone, Serialize)]
pub struct IperfArgs {
    /// The host id of where the iperf servers are running.
    #[clap(long = "server")]
    pub server: String,
    /// The host ids that will run iperf clients.
    #[clap(long, required = true, value_delimiter = ',', num_args = 1..)]
    pub clients: Vec<String>,
    /// The host id(s) of the hosts that will capture the wireless traffic.
    #[clap(long, required = true, value_delimiter = ',', num_args = 1..)]
    pub monitors: Vec<String>,
    /// In which direction to perform the IPerf tests.
    #[clap(short = 'D', long, default_value = "downlink")]
    pub direction: Direction,
    /// How the capture should last in seconds.
    #[clap(short = 'd', long, default_value = "10")]
    pub duration: u64,
    /// Whether to use UDP.
    #[clap(
        short = 'U',
        long = "udp",
        required = true,
        requires_if("true", "total_throughput")
    )]
    pub udp: Option<bool>,
    /// The total throughput that the clients should use together in bits per second.
    ///
    /// This will be divided equally over each client. Use 0 for unlimited throughput.
    #[clap(short = 'T', long = "throughput", default_value = "0")]
    pub total_throughput: u64,
    /// Configure the MCS.
    ///
    /// Follows the format of `iw dev <if> set bitrates <mcs...>`. For example: `he-mcs-5 1:11`.
    /// Set tp auto to use automatic MCS. Not providing a value will not set anything.
    #[clap(long)]
    pub mcs: Option<String>,
    /// The frequency the access point is using in MHz.
    #[clap(short = 'F', long)]
    pub frequency: u32,
    /// The bandwidth used by the AP in MHz.
    #[clap(short = 'B', long)]
    pub bandwidth: u32,
    /// The SSID (display name) of the access point.
    #[clap(long)]
    pub ssid: String,
    /// The BSSID of the access point, often the MAC address.
    #[clap(long)]
    pub bssid: String,
}

#[derive(ValueEnum, Debug, Clone, Copy, Serialize)]
pub enum Direction {
    Uplink,
    Downlink,
    Bidir,
}

pub async fn run(args: IperfArgs, hosts: Hosts, out_path: &Path) -> anyhow::Result<()> {
    let args_dump = {
        let config = PrettyConfig::new()
            .depth_limit(2)
            .separate_tuple_members(true)
            .enumerate_arrays(true);
        to_string_pretty(&args, config).context("failed to serialize args info")?
    };

    let total_bandwidth = args.total_throughput;
    let udp = args.udp.unwrap_or(true);

    let senders: Vec<_> = hosts
        .get_many(&args.clients)
        .map_err(|missing| anyhow!("no host with id {missing}"))?
        .collect();

    let access_point = hosts
        .get(&args.server)
        .context("access point id not found")?
        .clone();

    let Some(access_point_ifname) = access_point.extra_data.interface.clone() else {
        anyhow::bail!("Access point should have a wireless interface IP configured");
    };

    let server_ip = {
        debug!("Getting server ip");
        let output = access_point
            .session
            .shell(format!(
                "ip -4 a show {} | awk '/inet/ {{print $2}}' | cut -d/ -f1",
                access_point_ifname
            ))
            .output()
            .await
            .context("failed to get IP address of server")?;
        if !output.status.success() {
            anyhow::bail!(
                "failed to get IP address of server: returned with exit code {}",
                output.status
            );
        }

        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if s.is_empty() {
            anyhow::bail!("failed to get IP address of server: empty output");
        }
        debug!("Found server ip: {s}");
        s
    };

    tokio::fs::create_dir_all(&out_path)
        .await
        .expect("could not create output folder");
    // Write the arguments out in a file so they can be found later.
    tokio::fs::write(&out_path.join("arguments.ron"), &args_dump)
        .await
        .context("failed to save arguments")?;

    // Configure the MCS on the access point.
    // TODO: maybe make more general and also fix that this actually happens on the AP.
    if let Some(mcs) = args.mcs {
        debug!("Setting MCS");
        let output = access_point
            .session
            .shell(format!(
                "iw dev phy1-ap0 set bitrates {}",
                if &mcs.to_lowercase() == "auto" {
                    ""
                } else {
                    &mcs
                }
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
    }

    // Configure and start the monitoring.
    let monitor = MonitorConfig {
        ssid: args.ssid,
        bssid: args.bssid,
        monitors: args.monitors.clone(),
        targets: senders.iter().map(|v| v.id.clone()).collect(),
        // Give some extra leeway to ensure the monitor captures everything.
        duration: Duration::from_secs(args.duration + 4),
        output_path: Some(out_path.to_owned()),
        // TODO: how can this be automated in OpenWRT?
        frequency: args.frequency,
        bandwidth: args.bandwidth,
        set_aids: true,
    }
    .start(&hosts)
    .await
    .context("failed to start capture")?;

    let mut start_port = 5000;
    let iperf_client_num = senders.len();

    // Start the iperf servers on the access point.
    let access_point_ifname2 = access_point_ifname.clone();
    let aps = tokio::spawn(async move {
        info!("Starting iperf servers");
        let mut n = start_port;
        run_all(vec![&access_point; iperf_client_num], |_| {
            n += 1;
            format!("iperf3 -s --bind-dev {access_point_ifname2} -p {n} -1")
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
            "iperf3 -c {server_ip} -p {start_port} {0} -b {1} {2} {3}",
            // 0 - Bind interface
            h.extra_data
                .interface
                .as_ref()
                .map(|ifname| format!("--bind-dev {ifname}"))
                .unwrap_or_else(|| "".to_string()),
            // 1 - Bandwidth
            total_bandwidth / senders.len() as u64,
            // 2 - Use UDP or not
            if udp { "-u" } else { "" },
            // 3 - Which direction to test
            match args.direction {
                Direction::Uplink => "",
                Direction::Downlink => "-R",
                Direction::Bidir => "--bidir",
            },
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
    monitor.wait().await.expect("monitor task crashed");

    debug!("Waiting for AP to finish");
    select! {
        _ = tokio::time::sleep(Duration::from_secs(1)) => {
            // Close the remaining iperf sessions.
            _ = hosts
                .get(&args.server)
                .expect("access point was used earlier")
                .session
                .shell("killall iperf3")
                .output()
                .await;

            anyhow::bail!("AP iperf servers did not close correctly; remaining sessions killed");
        },
        result = aps => {
            _ = result.context("iperf on AP failed")?;
        },
    }

    Ok(())
}
