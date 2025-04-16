use std::path::Path;

use clap::Parser;

use crate::hosts::Hosts;

pub mod downlink;

#[derive(Parser, Debug, Clone)]
pub enum Script {
    Downlink(downlink::DowlinkArgs),
}

pub async fn run(args: Script, hosts: Hosts, out_path: &Path) -> anyhow::Result<()> {
    match args {
        Script::Downlink(args) => downlink::run(args, hosts, out_path).await,
    }
}
