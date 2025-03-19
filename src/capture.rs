use std::{
    io::{Cursor, Read},
    path::PathBuf,
    time::Duration,
};

use anyhow::Context;
use openssh::Stdio;
use tokio::fs::File;
use tracing::debug;

use crate::hosts::Host;

/// Defines options for capturing on a network interface.
#[derive(Debug)]
pub struct CaptureConfig {
    /// Name of the interface to capture on.
    pub interface: String,
    /// Determines when to stop the capture.
    pub stop_condition: StopCondition,
    /// The path to save the capture to. Especially useful if captures are expected to be large.
    ///
    /// The file provided path must not yet exists but its parent directory is expected to exist.
    pub output_path: Option<PathBuf>,
}

/// A condition to tell wireshark when to stop capturing.
#[derive(Debug)]
pub enum StopCondition {
    /// Stop after a certain duration. Accurate down to the number of seconds.
    Duration(Duration),
    /// Stop after capturing a certain amount of packets.
    Packets(u32),
}

/// A resulting wireless capture in pcapng format.
///
/// NOTE: this format is not checked after the capture and may contain invalid data.
#[derive(Debug)]
pub enum Capture {
    /// The capture is stored in a file.
    File(File),
    /// The capture is stored in memory.
    Buffer(Vec<u8>),
}

impl Host {
    /// Create a capture on a remote host and copy the capture over. Assumes wireshark (cli) is
    /// installed on the remote machine.
    pub async fn capture(&self, config: &CaptureConfig) -> anyhow::Result<Capture> {
        let mut result = match &config.output_path {
            Some(output_path) => {
                let file = File::create_new(output_path)
                    .await
                    .context("could not create capture output file")?;
                Capture::File(file)
            }
            None => Capture::Buffer(Vec::new()),
        };

        let stop_condition = match &config.stop_condition {
            StopCondition::Duration(duration) => format!("duration:{}", duration.as_secs()),
            StopCondition::Packets(packets) => format!("packets:{packets}"),
        };

        let mut capture = self
            .session
            .command("tshark")
            .arg("-F")
            .arg("pcapng")
            .arg("--interface")
            .arg(&config.interface)
            .arg("--autostop")
            .arg(stop_condition)
            .arg("-w")
            .arg("-") // Output the pcapng capture to the stdout.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .await
            .context("failed to start remote wireshark capture")?;

        // SAFETY: `Stdio::piped()` is used above for the stdout, so it should be present.
        let stdout = capture.stdout().as_mut().expect("missing stdout handle");
        // Write the stdout of the process (the capture file in this case) to a file or buffer.
        match &mut result {
            Capture::File(outfile) => {
                tokio::io::copy(stdout, outfile)
                    .await
                    .context("failed to write capture to file")?;
            }
            Capture::Buffer(items) => {
                tokio::io::copy(stdout, items)
                    .await
                    .context("failed to write capture to buffer")?;
            }
        }

        // Wait for the capture command to finish and ensure no error occurred.
        let output = capture
            .wait_with_output()
            .await
            .context("remote capture failed")?;
        if !output.status.success() {
            debug!(
                host = self.id,
                "Remote capture failed with status code {} and stderr output: \"{}\"",
                output.status,
                String::from_utf8_lossy(&output.stdout)
            );
            anyhow::bail!("remote capture failed with status {}", output.status);
        }

        Ok(result)
    }
}

impl Capture {
    pub async fn reader(self: Self) -> CaptureReader {
        match self {
            Capture::File(file) => CaptureReader::File(file.into_std().await),
            Capture::Buffer(items) => CaptureReader::Buffer(Cursor::new(items)),
        }
    }
}

pub enum CaptureReader {
    File(std::fs::File),
    Buffer(Cursor<Vec<u8>>),
}

impl Read for CaptureReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            CaptureReader::File(file) => file.read(buf),
            CaptureReader::Buffer(cursor) => cursor.read(buf),
        }
    }
}
