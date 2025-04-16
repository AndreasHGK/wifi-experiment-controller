pub mod capture;
pub mod connection;
pub mod driver;
pub mod hosts;
pub mod monitor;
pub mod package;

use std::{path::PathBuf, process::ExitCode, time::SystemTime};

use clap::Parser;
use controller::scripts::Script;
use controller::{hosts::HostsConfig, scripts};
use tracing::{debug, error};
use tracing_subscriber::EnvFilter;

/// Controller program for Wi-Fi experiments and benchmarks.
#[derive(Parser, Debug, Clone)]
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
    /// The path to write output to to.
    ///
    /// The `<timestamp>` placeholder can be used to fill in the current timestamp in seconds.
    #[clap(short = 'O', long = "out", default_value = "results/<timestamp>")]
    output_path: String,
    /// The specific script to run.
    #[command(subcommand)]
    script: Script,
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

    let out_path: PathBuf = {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .to_string();
        args.output_path.replace("<timestamp>", &now).into()
    };

    if let Err(err) = scripts::run(args.script, hosts, &out_path).await {
        error!("Script exited with an error: {err:?}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
