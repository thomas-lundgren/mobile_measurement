//! Wire packet codec — framed `(pkt_t, ver)` layout.
//!
//! See `contracts/packets/`. Every packet is `pkt_t: u8`, `ver: u8`, then a
//! fixed core whose width is determined by `(pkt_t, ver)`. Trailing bytes
//! beyond the core are ignored (realtime UDP padding). Big-endian throughout.
//!
//! The layouts — structs, `to_bytes` / `decode`, sizes, the `pkt_t` / `ver`
//! constants and their golden-vector tests — are generated from
//! `contracts/packets/layouts.yaml` into `packet/layouts.rs`. This file adds
//! what is not layout: errors, the activity names, v1 → v2, padding,
//! dispatch.

mod layouts;
pub use layouts::*;

/// Sentinel: field not yet filled by the remote (set in the first UE send).
pub const DSCP_UNKNOWN: u8 = 0xFF;

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
            PacketError::UnknownType(t, v) => {
                write!(f, "unknown packet (pkt_t={t:#04x}, ver={v:#04x})")
            }
        }
    }
}
impl std::error::Error for PacketError {}

impl RealtimePacket {
    /// Encode padded to `total` bytes (>= WIRE_LEN) with trailing zeros, to hit
    /// a flow's target `packet_size`.
    pub fn to_bytes_padded(&self, total: usize) -> Vec<u8> {
        let mut v = self.to_bytes().to_vec();
        if total > v.len() {
            v.resize(total, 0);
        }
        v
    }
}

impl AppRequest {
    pub const DOWNLOAD: u8 = 0;
    pub const UPLOAD: u8 = 1;
}

/// Activity codes on the wire (the `activity` byte of `AppRequest` and
/// `AppExchange`: the index) and the names both nodes log for them
/// (`enums.yaml` app_activities, in order).
pub const APP_ACTIVITIES: [&str; 15] = [
    "other",
    "web_page",
    "web_object",
    "mail_sync",
    "mail_attachment",
    "mail_send",
    "video_chunk",
    "feed_image",
    "photo_upload",
    "llm_call",
    "mcp_call",
    "web_fetch",
    "cloud_tool",
    "llm_resume",
    "web_search",
];

pub fn app_activity_code(name: &str) -> u8 {
    APP_ACTIVITIES.iter().position(|a| *a == name).unwrap_or(0) as u8
}

pub fn app_activity_name(code: u8) -> &'static str {
    APP_ACTIVITIES
        .get(code as usize)
        .copied()
        .unwrap_or("other")
}

impl AppExchange {
    /// The v2 equivalent of a v1 request.
    pub fn from_v1(r: &AppRequest) -> Self {
        let (up_bytes, down_bytes) = if r.kind == AppRequest::UPLOAD {
            (r.size, 0)
        } else {
            (0, r.size)
        };
        Self {
            activity: r.activity,
            flow_id: r.flow_id,
            object_id: r.object_id,
            up_bytes,
            down_bytes,
            delay_ms: 0,
            rate_bps: 0,
            chunk_bytes: 0,
        }
    }
}

/// Either packet type, dispatched on the header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packet {
    Probe(ProbePacket),
    Realtime(RealtimePacket),
    DscpProbe(DscpProbePacket),
}

impl Packet {
    pub fn decode(buf: &[u8]) -> Result<Self, PacketError> {
        if buf.len() < 2 {
            return Err(PacketError::ShortBuffer);
        }
        match (buf[0], buf[1]) {
            (PKT_PROBE, VER_1) => Ok(Packet::Probe(ProbePacket::decode(buf)?)),
            (PKT_REALTIME, VER_1) => Ok(Packet::Realtime(RealtimePacket::decode(buf)?)),
            (PKT_DSCP_PROBE, VER_1) => Ok(Packet::DscpProbe(DscpProbePacket::decode(buf)?)),
            (t, v) => Err(PacketError::UnknownType(t, v)),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn app_activity_names_round_trip() {
        for (i, name) in APP_ACTIVITIES.iter().enumerate() {
            assert_eq!(app_activity_code(name) as usize, i);
            assert_eq!(app_activity_name(i as u8), *name);
        }
        assert_eq!(app_activity_code("nonsense"), 0);
    }

    /// The wire table is `enums.yaml` app_activities, in order (code = index).
    #[test]
    fn app_activities_match_the_contract() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../contracts/enums/enums.yaml");
        let doc: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let want: Vec<&str> = doc["app_activities"]
            .as_sequence()
            .expect("enums.yaml app_activities")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(APP_ACTIVITIES.as_slice(), want.as_slice());
    }

    use super::*;

    #[test]
    fn app_request_round_trips() {
        let r = AppRequest {
            kind: AppRequest::UPLOAD,
            activity: 5,
            flow_id: 7,
            object_id: 1 << 48 | 3,
            size: 2_500_000,
        };
        let b = r.to_bytes();
        assert_eq!(&b[..4], &[0x04, 0x01, 0x01, 0x05]);
        assert_eq!(AppRequest::decode(&b), Ok(r));
        assert_eq!(AppRequest::decode(&b[..10]), Err(PacketError::ShortBuffer));
        assert_eq!(
            AppRequest::decode(&[0x02, 0x01, 0, 0]),
            Err(PacketError::UnknownType(2, 1))
        );
    }
    #[test]
    fn app_exchange_round_trips_and_reads_v1() {
        let x = AppExchange {
            activity: 9,
            flow_id: 7,
            object_id: 5 << 20 | 2,
            up_bytes: 40_000,
            down_bytes: 12_000,
            delay_ms: 900,
            rate_bps: 30_000,
            chunk_bytes: 140,
        };
        let b = x.to_bytes();
        assert_eq!(&b[..4], &[0x04, 0x02, 0x09, 0x00]);
        assert_eq!(AppExchange::decode(&b), Ok(x));
        assert_eq!(AppExchange::decode(&b[..43]), Err(PacketError::ShortBuffer));
        assert_eq!(
            AppExchange::decode(&[0x04, 0x01, 0, 0]),
            Err(PacketError::UnknownType(4, 1))
        );
        let up = AppRequest {
            kind: AppRequest::UPLOAD,
            activity: 8,
            flow_id: 3,
            object_id: 1,
            size: 500,
        };
        let v = AppExchange::from_v1(&up);
        assert_eq!(
            (v.up_bytes, v.down_bytes, v.delay_ms, v.rate_bps),
            (500, 0, 0, 0)
        );
    }

    #[test]
    fn probe_roundtrip() {
        let pkt = ProbePacket {
            flow_id: 42,
            sequence_number: 1_234_567_890,
            send_mono_ns: 98_765_432_100,
        };
        assert_eq!(ProbePacket::decode(&pkt.to_bytes()).unwrap(), pkt);
    }

    #[test]
    fn realtime_roundtrip_and_padding() {
        let pkt = RealtimePacket {
            flow_id: 7,
            sequence_number: 42,
            send_mono_ns: 1_000_000_000,
            send_utc_ns: 1_747_900_000_000_000_000,
        };
        assert_eq!(RealtimePacket::decode(&pkt.to_bytes()).unwrap(), pkt);
        let padded = pkt.to_bytes_padded(64);
        assert_eq!(padded.len(), 64);
        // trailing padding is ignored on decode
        assert_eq!(RealtimePacket::decode(&padded).unwrap(), pkt);
    }

    #[test]
    fn rejects_unknown_and_short() {
        assert_eq!(
            Packet::decode(&[0xff, 0x01, 0, 0]),
            Err(PacketError::UnknownType(0xff, 0x01))
        );
        assert_eq!(
            ProbePacket::decode(&[PKT_PROBE, VER_1, 0, 0]),
            Err(PacketError::ShortBuffer)
        );
        assert_eq!(Packet::decode(&[0x01]), Err(PacketError::ShortBuffer));
    }

    #[test]
    fn dscp_probe_roundtrip() {
        let pkt = DscpProbePacket {
            flow_id: 1,
            sequence_number: 0,
            send_mono_ns: 1_000_000_000,
            dscp_intended_ul: 46,
            dscp_actual_ul: DSCP_UNKNOWN,
            dscp_intended_dl: DSCP_UNKNOWN,
        };
        assert_eq!(DscpProbePacket::decode(&pkt.to_bytes()).unwrap(), pkt);
        // Echo: server fills in observed values
        let echo = DscpProbePacket {
            dscp_actual_ul: 0,
            dscp_intended_dl: 46,
            ..pkt
        };
        assert_eq!(DscpProbePacket::decode(&echo.to_bytes()).unwrap(), echo);
    }
}
