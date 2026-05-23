//! Wire packet codec — framed `(pkt_t, ver)` layout.
//!
//! See `contracts/packets/`. Every packet is `pkt_t: u8`, `ver: u8`, then a
//! fixed core whose width is determined by `(pkt_t, ver)`. Trailing bytes
//! beyond the core are ignored (realtime UDP padding). Big-endian throughout.
//! Golden-vector conformance lives in the tests below.

/// Packet type discriminators.
pub const PKT_PROBE: u8 = 0x01;
pub const PKT_REALTIME: u8 = 0x02;
/// Initial per-type format version.
pub const VER_1: u8 = 0x01;

/// Decode failures (typed — never panic on bad input).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketError {
    /// Fewer bytes than the header + core for this `(pkt_t, ver)`.
    ShortBuffer,
    /// `(pkt_t, ver)` not in the size table.
    UnknownType(u8, u8),
}

impl std::fmt::Display for PacketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PacketError::ShortBuffer => write!(f, "short packet buffer"),
            PacketError::UnknownType(t, v) => write!(f, "unknown packet (pkt_t={t:#04x}, ver={v:#04x})"),
        }
    }
}
impl std::error::Error for PacketError {}

/// Probe packet (`pkt_t = 0x01`, `ver = 0x01`). 20-byte core, 22 on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbePacket {
    pub flow_id: u32,
    pub sequence_number: u64,
    pub send_mono_ns: u64,
}

impl ProbePacket {
    pub const CORE_LEN: usize = 20;
    pub const WIRE_LEN: usize = 2 + Self::CORE_LEN; // 22

    pub fn to_bytes(&self) -> [u8; Self::WIRE_LEN] {
        let mut b = [0u8; Self::WIRE_LEN];
        b[0] = PKT_PROBE;
        b[1] = VER_1;
        b[2..6].copy_from_slice(&self.flow_id.to_be_bytes());
        b[6..14].copy_from_slice(&self.sequence_number.to_be_bytes());
        b[14..22].copy_from_slice(&self.send_mono_ns.to_be_bytes());
        b
    }

    /// Decode from a buffer that begins with the 2-byte header. Trailing bytes
    /// beyond the core are ignored.
    pub fn decode(buf: &[u8]) -> Result<Self, PacketError> {
        if buf.len() < 2 {
            return Err(PacketError::ShortBuffer);
        }
        if buf[0] != PKT_PROBE || buf[1] != VER_1 {
            return Err(PacketError::UnknownType(buf[0], buf[1]));
        }
        if buf.len() < Self::WIRE_LEN {
            return Err(PacketError::ShortBuffer);
        }
        Ok(Self {
            flow_id: u32::from_be_bytes(buf[2..6].try_into().unwrap()),
            sequence_number: u64::from_be_bytes(buf[6..14].try_into().unwrap()),
            send_mono_ns: u64::from_be_bytes(buf[14..22].try_into().unwrap()),
        })
    }
}

/// Realtime packet (`pkt_t = 0x02`, `ver = 0x01`). 28-byte core, 30 on the wire
/// (minimum); padded with trailing zero bytes to a target `packet_size`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RealtimePacket {
    pub flow_id: u32,
    pub sequence_number: u64,
    pub send_mono_ns: u64,
    pub send_utc_ns: u64,
}

impl RealtimePacket {
    pub const CORE_LEN: usize = 28;
    pub const WIRE_LEN: usize = 2 + Self::CORE_LEN; // 30

    pub fn to_bytes(&self) -> [u8; Self::WIRE_LEN] {
        let mut b = [0u8; Self::WIRE_LEN];
        b[0] = PKT_REALTIME;
        b[1] = VER_1;
        b[2..6].copy_from_slice(&self.flow_id.to_be_bytes());
        b[6..14].copy_from_slice(&self.sequence_number.to_be_bytes());
        b[14..22].copy_from_slice(&self.send_mono_ns.to_be_bytes());
        b[22..30].copy_from_slice(&self.send_utc_ns.to_be_bytes());
        b
    }

    /// Encode padded to `total` bytes (>= WIRE_LEN) with trailing zeros, to hit
    /// a flow's target `packet_size`.
    pub fn to_bytes_padded(&self, total: usize) -> Vec<u8> {
        let mut v = self.to_bytes().to_vec();
        if total > v.len() {
            v.resize(total, 0);
        }
        v
    }

    pub fn decode(buf: &[u8]) -> Result<Self, PacketError> {
        if buf.len() < 2 {
            return Err(PacketError::ShortBuffer);
        }
        if buf[0] != PKT_REALTIME || buf[1] != VER_1 {
            return Err(PacketError::UnknownType(buf[0], buf[1]));
        }
        if buf.len() < Self::WIRE_LEN {
            return Err(PacketError::ShortBuffer);
        }
        Ok(Self {
            flow_id: u32::from_be_bytes(buf[2..6].try_into().unwrap()),
            sequence_number: u64::from_be_bytes(buf[6..14].try_into().unwrap()),
            send_mono_ns: u64::from_be_bytes(buf[14..22].try_into().unwrap()),
            send_utc_ns: u64::from_be_bytes(buf[22..30].try_into().unwrap()),
        })
    }
}

/// Either packet type, dispatched on the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packet {
    Probe(ProbePacket),
    Realtime(RealtimePacket),
}

impl Packet {
    pub fn decode(buf: &[u8]) -> Result<Self, PacketError> {
        if buf.len() < 2 {
            return Err(PacketError::ShortBuffer);
        }
        match (buf[0], buf[1]) {
            (PKT_PROBE, VER_1) => Ok(Packet::Probe(ProbePacket::decode(buf)?)),
            (PKT_REALTIME, VER_1) => Ok(Packet::Realtime(RealtimePacket::decode(buf)?)),
            (t, v) => Err(PacketError::UnknownType(t, v)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn probe_roundtrip() {
        let pkt = ProbePacket { flow_id: 42, sequence_number: 1_234_567_890, send_mono_ns: 98_765_432_100 };
        assert_eq!(ProbePacket::decode(&pkt.to_bytes()).unwrap(), pkt);
    }

    #[test]
    fn realtime_roundtrip_and_padding() {
        let pkt = RealtimePacket { flow_id: 7, sequence_number: 42, send_mono_ns: 1_000_000_000, send_utc_ns: 1_747_900_000_000_000_000 };
        assert_eq!(RealtimePacket::decode(&pkt.to_bytes()).unwrap(), pkt);
        let padded = pkt.to_bytes_padded(64);
        assert_eq!(padded.len(), 64);
        // trailing padding is ignored on decode
        assert_eq!(RealtimePacket::decode(&padded).unwrap(), pkt);
    }

    #[test]
    fn rejects_unknown_and_short() {
        assert_eq!(Packet::decode(&[0xff, 0x01, 0, 0]), Err(PacketError::UnknownType(0xff, 0x01)));
        assert_eq!(ProbePacket::decode(&[PKT_PROBE, VER_1, 0, 0]), Err(PacketError::ShortBuffer));
        assert_eq!(Packet::decode(&[0x01]), Err(PacketError::ShortBuffer));
    }

    fn hex_to_bytes(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    fn vectors_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../contracts/testvectors/packets")
    }

    #[test]
    fn golden_probe_v1() {
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(vectors_dir().join("probe_v1.json")).unwrap()).unwrap();
        for case in doc["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let d = &case["decoded"];
            let pkt = ProbePacket {
                flow_id: d["flow_id"].as_u64().unwrap() as u32,
                sequence_number: d["sequence_number"].as_u64().unwrap(),
                send_mono_ns: d["send_mono_ns"].as_u64().unwrap(),
            };
            let expected = hex_to_bytes(case["hex"].as_str().unwrap());
            assert_eq!(pkt.to_bytes().to_vec(), expected, "encode `{name}`");
            assert_eq!(ProbePacket::decode(&expected).unwrap(), pkt, "decode `{name}`");
        }
    }

    #[test]
    fn golden_realtime_v1() {
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(vectors_dir().join("realtime_v1.json")).unwrap()).unwrap();
        for case in doc["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let d = &case["decoded"];
            let pkt = RealtimePacket {
                flow_id: d["flow_id"].as_u64().unwrap() as u32,
                sequence_number: d["sequence_number"].as_u64().unwrap(),
                send_mono_ns: d["send_mono_ns"].as_u64().unwrap(),
                send_utc_ns: d["send_utc_ns"].as_u64().unwrap(),
            };
            let expected = hex_to_bytes(case["hex"].as_str().unwrap());
            let pad = case["trailing_pad_bytes"].as_u64().unwrap_or(0) as usize;
            assert_eq!(pkt.to_bytes_padded(RealtimePacket::WIRE_LEN + pad), expected, "encode `{name}`");
            assert_eq!(RealtimePacket::decode(&expected).unwrap(), pkt, "decode `{name}`");
        }
    }
}
