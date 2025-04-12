pub mod capture;
pub mod connection;
pub mod driver;
pub mod hosts;
pub mod monitor;
pub mod package;

use std::{
    path::PathBuf,
    process::ExitCode,
    time::{Duration, SystemTime},
};

use clap::Parser;
use controller::monitor::MonitorConfig;
use controller::{hosts::HostsConfig, utils::run_all};
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tracing::{debug, error, info};
use tracing_subscriber::EnvFilter;

/// Controller program for Wi-Fi experiments and benchmarks.
#[derive(Parser, Debug)]
#[command(about)]
struct Args {
    /// Sets the logging verbosity.
    ///
    /// Can be: `trace`, `debug`, `info`, `warn`, `error`. Per-module directives can also be used,
    /// for example: `info,controller=debug.`
    #[arg(short = 'L', long, env, default_value = "INFO")]
    log_level: String,
    /// Hosts configuration file path.
    #[clap(short = 'H', long, value_parser, default_value = "./hosts.toml")]
    hosts_file: String,
}

#[tokio::main]
async fn main() -> ExitCode {
    // Parse command-line arguments based on the [Args] struct.
    let args = Args::parse();

    // Set up human-readable logging using the `tracing-subcriber` crate.
    tracing_subscriber::fmt()
        .with_env_filter(match EnvFilter::builder().parse(args.log_level) {
            Ok(v) => v,
            Err(err) => {
                eprintln!("Failed to parse log_level argument: {err:?}");
                return ExitCode::FAILURE;
            }
        })
        .init();
    debug!("Debug logging is enabled");

    let hosts_config = match HostsConfig::read(&args.hosts_file).await {
        Ok(v) => v,
        Err(err) => {
            error!("Unable to parse `{}`: {err}", args.hosts_file);
            return ExitCode::FAILURE;
        }
    };

    let hosts = match hosts_config.connect().await {
        Ok(v) => v,
        Err(err) => {
            error!("Could not initialize ssh connections: {err:?}");
            return ExitCode::FAILURE;
        }
    };

    let monitors = ["idlab50".to_string()];
    let senders = [
        "tsn11".to_string(),
        "idlab51".to_string(),
        "idlab52".to_string(),
        "tsn01".to_string(),
        "tsn02".to_string(),
        "tsn10".to_string(),
    ];

    let results_folder: PathBuf = format!(
        "results/{}",
        // SAFETY: This would only panic if time went backwards.
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    )
    .into();

    tokio::fs::create_dir_all(&results_folder)
        .await
        .expect("could not create output folder");

    let monitor = MonitorConfig {
        ssid: "OpenWrt".to_string(),
        bssid: "10:7c:61:df:7a:d2".to_string(),
        monitors: monitors[0..1].to_vec(),
        targets: senders.to_vec(),
        duration: Duration::from_secs(15),
        output_path: Some(results_folder.clone()),
        frequency: 5580,
        bandwidth: 80,
        set_aids: true,
    }
    .start(&hosts)
    .await
    .expect("failed to start capture");

    let mut start_port = 2550;
    let iperf_client_num = senders.len();
    let access_point = hosts.get("ap").unwrap().clone();

    // Start the iperf servers on the access point.
    tokio::spawn(async move {
        info!("Starting iperf servers");
        let mut n = start_port;
        run_all(vec![&access_point; iperf_client_num], |_| {
            n += 1;
            format!("iperf3 -s 192.168.1.1 -p {n} -1")
        })
        .await
        .unwrap();
    });

    // Run iperf clients on each NUC.
    info!("Starting iperf clients");
    let bitrates = [
        10_000_000, 30_000_000, 20_000_000, 10_000_000, 30_000_000, 20_000_000,
    ];
    let mut n = 0;
    let iperfs = run_all(
        hosts
            .get_many(senders.iter().map(|s| s.as_str()))
            .expect("valid hosts"),
        |_| {
            let br = bitrates[0];
            n += 1;
            start_port += 1;
            format!("iperf3 -c 192.168.1.1 -p {start_port} --R -u -b {br}")
        },
    )
    .await
    .unwrap();

    // Write all the iperf outputs to files.
    for (num, iperf) in iperfs.into_iter().enumerate() {
        let mut f = File::create_new(results_folder.join(&format!("{num}.txt")))
            .await
            .unwrap();
        f.write_all(&iperf.stdout).await.unwrap();
    }

    monitor.wait().await.unwrap();

    ExitCode::SUCCESS
}
