//! `contracts/schemas/test_plan.schema.json` — the port-less plan + bindings.

use crate::enums::Direction;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The port-less recipe for one test. Server-allocated ports travel separately
/// as `FlowBinding`s (see RunTestPayload / header).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TestPlan {
    pub name: String,
    pub version: u32,
    pub duration_sec: u32,
    pub flows: Vec<Flow>,
}

/// A flow, discriminated by `type`. Port-less — `Flow` carries no server_port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type")]
pub enum Flow {
    #[serde(rename = "probe")]
    Probe {
        flow_id: u32,
        rate_pps: u32,
        #[serde(default = "default_probe_packet_size")]
        packet_size_bytes: u32,
    },
    #[serde(rename = "realtime_udp")]
    RealtimeUdp {
        flow_id: u32,
        direction: Direction,
        rate_pps: u32,
        packet_size_bytes: u32,
    },
    #[serde(rename = "bulk_tcp")]
    BulkTcp {
        flow_id: u32,
        direction: Direction,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        target_bytes: Option<u32>,
    },
    /// Logger-only: ICMP/UDP/TCP-SYN probes to auto-discovered first-hop gateways.
    /// No server endpoint — FlowBinding.server_port is None for this type.
    #[serde(rename = "gateway_ping")]
    GatewayPing {
        flow_id: u32,
        /// Probe cadence in ms. Default 1000.
        #[serde(default = "default_ping_interval_ms", skip_serializing_if = "is_default_ping_interval")]
        ping_interval_ms: u32,
    },

    /// DSCP transparency probe. UE cycles through dscp_values marking each outgoing
    /// packet; server echoes with observed + intended DSCP filled into the payload.
    /// See contracts/packets/dscp_probe.md.
    #[serde(rename = "dscp_probe")]
    DscpProbe {
        flow_id: u32,
        /// DSCP values (0–63) to cycle through, one per packet (seq % len).
        dscp_values: Vec<u8>,
        #[serde(default = "default_dscp_rate_pps", skip_serializing_if = "is_default_dscp_rate")]
        rate_pps: u32,
    },
}

impl Flow {
    /// The flow's plan-scoped id, regardless of type.
    pub fn flow_id(&self) -> u32 {
        match *self {
            Flow::Probe { flow_id, .. }
            | Flow::RealtimeUdp { flow_id, .. }
            | Flow::BulkTcp { flow_id, .. }
            | Flow::GatewayPing { flow_id, .. }
            | Flow::DscpProbe { flow_id, .. } => flow_id,
        }
    }
}

fn default_probe_packet_size() -> u32 { 22 }
fn default_ping_interval_ms() -> u32 { 1000 }
fn is_default_ping_interval(v: &u32) -> bool { *v == 1000 }
fn default_dscp_rate_pps() -> u32 { 10 }
fn is_default_dscp_rate(v: &u32) -> bool { *v == 10 }

/// Server-allocated binding for one flow, keyed by flow_id. Lives in
/// RunTestPayload / header, not in the plan. `server_port` is absent for
/// gateway_ping flows (no server endpoint).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FlowBinding {
    pub flow_id: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_port: Option<u16>,
}
