use std::io::{Read, Write};

/// Fixed-width probe packet layout (big-endian / network byte order).
///
/// | Offset | Width | Field                |
/// |--------|-------|----------------------|
/// | 0      | 4     | flow_id (u32)        |
/// | 4      | 8     | sequence_number (u64)|
/// | 12     | 8     | send_mono_ns (u64)   |
/// Total: 20 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbePacket {
    pub flow_id: u32,
    pub sequence_number: u64,
    pub send_mono_ns: u64,
}

impl ProbePacket {
    pub const SIZE: usize = 20;

    pub fn encode(&self, buf: &mut [u8; Self::SIZE]) {
        buf[0..4].copy_from_slice(&self.flow_id.to_be_bytes());
        buf[4..12].copy_from_slice(&self.sequence_number.to_be_bytes());
        buf[12..20].copy_from_slice(&self.send_mono_ns.to_be_bytes());
    }

    pub fn decode(buf: &[u8; Self::SIZE]) -> Self {
        let flow_id = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let sequence_number = u64::from_be_bytes([
            buf[4], buf[5], buf[6], buf[7], buf[8], buf[9], buf[10], buf[11],
        ]);
        let send_mono_ns = u64::from_be_bytes([
            buf[12], buf[13], buf[14], buf[15], buf[16], buf[17], buf[18], buf[19],
        ]);
        Self {
            flow_id,
            sequence_number,
            send_mono_ns,
        }
    }

    pub fn to_bytes(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        self.encode(&mut buf);
        buf
    }
}

/// Read exactly `ProbePacket::SIZE` bytes from `r`.
pub fn read_probe<R: Read>(r: &mut R) -> std::io::Result<ProbePacket> {
    let mut buf = [0u8; ProbePacket::SIZE];
    r.read_exact(&mut buf)?;
    Ok(ProbePacket::decode(&buf))
}

/// Write a `ProbePacket` to `w`.
pub fn write_probe<W: Write>(w: &mut W, pkt: &ProbePacket) -> std::io::Result<()> {
    w.write_all(&pkt.to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let pkt = ProbePacket {
            flow_id: 42,
            sequence_number: 1_234_567_890,
            send_mono_ns: 98_765_432_100,
        };
        let bytes = pkt.to_bytes();
        let decoded = ProbePacket::decode(&bytes);
        assert_eq!(pkt, decoded);
    }
}
