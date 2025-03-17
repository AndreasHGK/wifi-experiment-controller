use anyhow::Context;
use pcap_parser::{traits::PcapReaderIterator, PcapNGReader};

use super::CaptureReader;

// TODO: why not use tshark -Tfields --interface mon0 -e "wlan.fixed.aid" -Y "wlan.fc.type_subtype == 0x0001 && wlan.bssid == 10:7c:61:df:7a:d2"

pub fn extract_aids(capture: CaptureReader, ssid: &str) -> anyhow::Result<Vec<u16>> {
    let mut reader = PcapNGReader::new(65536, capture).context("could not create pcapng reader")?;
    loop {}

    todo!()
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_extract_aids() {
        todo!()
    }
}
