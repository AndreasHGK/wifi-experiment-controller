use std::{path::PathBuf, time::Duration};

use anyhow::Context;
use tokio::task::JoinSet;

use crate::{
    capture::{Capture, CaptureConfig, StopCondition},
    hosts::{HostId, Hosts},
};

pub struct MonitorConfig {
    /// The SSID of the network to monitor.
    pub ssid: String,
    pub monitors: Vec<HostId>,
    pub duration: Duration,
    /// Where to write the captures to.
    pub output_path: Option<PathBuf>,
}

impl MonitorConfig {
    /// Start monitoring traffic.
    pub async fn start(self: Self, hosts: &Hosts) -> anyhow::Result<Monitor> {
        let monitor_hosts = hosts
            .get_many(self.monitors.iter().map(|v| v.as_str()))?
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();

        // Connect the non monitor hosts and determine their association ID.
        let connected_hosts = hosts
            .all_except(self.monitors.iter().map(|v| v.as_str()))
            .cloned();

        // Set up the actual capture that will find the association ids.
        // TODO: stop the capture as soon as all hosts are connected instead of just capturing for
        // a set amount of time.
        let capture_host = monitor_hosts[0].clone();
        let capture = tokio::spawn(async move {
            capture_host
                .capture(&CaptureConfig {
                    interface: "mon0".to_string(),
                    stop_condition: StopCondition::Duration(Duration::from_secs(1)),
                    output_path: None,
                })
                .await
        });

        let mut connection_join_set = JoinSet::new();
        for connected_host in connected_hosts {
            let ssid = self.ssid.clone();
            connection_join_set.spawn(async move { connected_host.associate(&ssid, None).await });
        }
        for result in connection_join_set.join_all().await {
            result?;
        }
        capture.await.context("failed to capture AIDs")?;

        let mut captures = JoinSet::new();
        for monitor_host in monitor_hosts {
            let output_path = self.output_path.clone();
            captures.spawn(async move {
                monitor_host
                    .capture(&CaptureConfig {
                        interface: "mon0".to_string(),
                        stop_condition: StopCondition::Duration(self.duration),
                        output_path: output_path
                            .map(|v| v.join(&monitor_host.id).with_extension("pcapng")),
                    })
                    .await
                    .map(|res| (monitor_host.id.clone(), res))
            });
        }
        Ok(Monitor { captures })
    }
}

pub struct Monitor {
    captures: JoinSet<anyhow::Result<(HostId, Capture)>>,
}

impl Monitor {
    /// Waits for all the captures to complete and returns their results.
    pub async fn wait(self: Self) -> anyhow::Result<Vec<(HostId, Capture)>> {
        self.captures
            .join_all()
            .await
            .into_iter()
            .try_fold(Vec::new(), |mut acc, item| {
                acc.push(item.context("capture returned an error")?);
                Ok(acc)
            })
    }

    /// Immediately stops the captures, throwing away the results.
    pub fn abort(self: &mut Self) {
        self.captures.abort_all();
    }
}
