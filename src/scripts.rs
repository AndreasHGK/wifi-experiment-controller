use std::path::Path;

use clap::Parser;

use crate::hosts::Hosts;

pub mod iperf;

#[derive(Parser, Debug, Clone)]
pub enum Script {
    /// Run an IPerf stress test with multiple nodes.
    Iperf(iperf::IperfArgs),
}

pub async fn run(args: Script, hosts: Hosts, out_path: &Path) -> anyhow::Result<()> {
    match args {
        Script::Iperf(args) => iperf::run(args, hosts, out_path).await,
    }
}
