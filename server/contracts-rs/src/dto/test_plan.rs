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
}

impl Flow {
    /// The flow's plan-scoped id, regardless of type.
    pub fn flow_id(&self) -> u32 {
        match *self {
            Flow::Probe { flow_id, .. }
            | Flow::RealtimeUdp { flow_id, .. }
            | Flow::BulkTcp { flow_id, .. } => flow_id,
        }
    }
}

fn default_probe_packet_size() -> u32 {
    22
}

/// Server-allocated port for one flow, keyed by flow_id. Lives in
/// RunTestPayload / header, not in the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FlowBinding {
    pub flow_id: u32,
    pub server_port: u16,
}
