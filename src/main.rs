pub mod capture;
pub mod connection;
pub mod driver;
pub mod hosts;
pub mod monitor;
pub mod package;

use std::{process::ExitCode, time::Duration};

use clap::Parser;
use hosts::HostsConfig;
use monitor::MonitorConfig;
use tracing::{debug, error};
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

    let monitor = MonitorConfig {
        ssid: "OpenWrt".to_string(),
        bssid: "10:7c:61:df:7a:d2".to_string(),
        monitors: vec![
            "nuc1".to_string(),
            "nuc5".to_string(),
            "nuc6".to_string(),
            "nuc7".to_string(),
        ],
        duration: Duration::from_secs(1),
        output_path: Some("./results".into()),
        set_aids: true,
    }
    .start(&hosts)
    .await
    .expect("failed to start capture");

    monitor.wait().await.unwrap();

    ExitCode::SUCCESS
}
