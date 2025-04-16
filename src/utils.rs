use std::{process::Output, sync::Arc};

use anyhow::Context;
use tokio::task::JoinSet;
use tracing::error;

use crate::hosts::Host;

pub async fn run_all<F>(
    hosts: impl IntoIterator<Item = &Arc<Host>>,
    mut func: F,
) -> anyhow::Result<Vec<(Arc<Host>, Output)>>
where
    F: FnMut(&Arc<Host>) -> String,
{
    let mut commands = JoinSet::new();

    hosts.into_iter().for_each(|host| {
        let host = host.clone();
        let command = func(&host);
        commands.spawn(async move { (host.clone(), host.session.shell(command).output().await) });
    });

    let mut out = Vec::new();
    for (host, result) in commands.join_all().await {
        let result = match result {
            Ok(v) => (host, v),
            Err(err) => {
                error!(host = host.id, "running command failed: {err}");
                return Err(err).context("failed to run command");
            }
        };

        out.push(result);
    }

    Ok(out)
}
