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
        /// Sender only: limit SO_SNDBUF to this many bytes and send
        /// non-blocking (back-pressure test; full buffer → EAGAIN, logged).
        #[serde(skip_serializing_if = "Option::is_none", default)]
        sndbuf_bytes: Option<u32>,
    },
    #[serde(rename = "bulk_tcp")]
    BulkTcp {
        flow_id: u32,
        direction: Direction,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        target_bytes: Option<u32>,
        /// Sender only: pace writes to this rate (kbit/s, application bytes)
        /// instead of sending as fast as TCP allows. Omit for uncapped bulk.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        rate_kbps: Option<u32>,
    },
    /// Logger-only: ICMP/UDP/TCP-SYN probes to auto-discovered first-hop gateways.
    /// No server endpoint — FlowBinding.server_port is None for this type.
    #[serde(rename = "gateway_ping")]
    GatewayPing {
        flow_id: u32,
        /// Probe cadence in ms. Default 1000.
        #[serde(
            default = "default_ping_interval_ms",
            skip_serializing_if = "is_default_ping_interval"
        )]
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
        #[serde(
            default = "default_dscp_rate_pps",
            skip_serializing_if = "is_default_dscp_rate"
        )]
        rate_pps: u32,
    },

    /// App-like background load: simulated users on the UE request objects
    /// over TCP from the server (contracts/packets/app_request.md).
    #[serde(rename = "app_traffic")]
    AppTraffic {
        flow_id: u32,
        /// User model: web | mail | video | social | mixed | script.
        profile: AppProfile,
        /// Concurrent users (1–16, default 1). A script runs on one.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        workers: Option<u32>,
        /// Seed of the users' choices (default 1): same seed, same requests.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        seed: Option<u64>,
        /// Multiplies every object size (default 1.0); < 1 saves data.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        scale: Option<f64>,
        /// profile "script" only: the timeline to run.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        actions: Option<Vec<AppAction>>,
        /// profiles "agent", "websearch": where the tools run (default device).
        #[serde(skip_serializing_if = "Option::is_none", default)]
        placement: Option<AppToolPlacement>,
        /// profiles "agent", "websearch": closed (default) or open loop.
        #[serde(rename = "loop", skip_serializing_if = "Option::is_none", default)]
        loop_mode: Option<AppLoop>,
        /// loop open: mean seconds between task arrivals per user (default 30).
        #[serde(skip_serializing_if = "Option::is_none", default)]
        task_interval_s: Option<f64>,
        /// loop open: unfinished tasks per user before an arriving one is
        /// dropped (default 4).
        #[serde(skip_serializing_if = "Option::is_none", default)]
        max_open_tasks: Option<u32>,
    },
}

/// Built-in user models of an app_traffic flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AppProfile {
    Web,
    Mail,
    Video,
    Social,
    Mixed,
    /// An AI agent: LLM calls (context up, streamed reply) and tool calls
    /// (MCP, web fetch) in task loops.
    Agent,
    /// An AI agent answering by web search: per task a graph of an LLM plan
    /// call, a search, parallel page fetches and an LLM synthesis call.
    Websearch,
    Script,
}

/// How an app_traffic flow's work arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AppLoop {
    /// The next task starts after the last one ended (and the user read it).
    Closed,
    /// Tasks arrive on a schedule; objects start when due.
    Open,
}

/// Where an AI agent's tools run (app_traffic profiles "agent", "websearch").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AppToolPlacement {
    /// On the phone: tool calls are phone traffic; each LLM call after one
    /// sends the results up with the context.
    Device,
    /// At the LLM provider, inside the call: the reply pauses while the tool
    /// runs and carries the result text down; no tool traffic of its own.
    Cloud,
    /// MCP calls on the phone, web fetches in the cloud.
    Hybrid,
}

/// One step of a scripted app_traffic timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AppAction {
    /// Start this long after the test starts; actions run in order.
    pub at_ms: u64,
    pub kind: AppActionKind,
    /// Size of each object.
    pub bytes: u64,
    /// Number of objects (default 1).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub count: Option<u32>,
    /// Objects in flight at once (default 1, at most 6).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub parallel: Option<u32>,
    /// Activity name for the logs (contracts/enums/enums.yaml app_activities).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub activity: Option<String>,
    /// kind "down": bytes sent up first (request body, LLM context).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub up_bytes: Option<u64>,
    /// kind "down": server think time before the first byte (ms).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub delay_ms: Option<u32>,
    /// kind "down": pace the response at this rate, like a token stream.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub rate_kbps: Option<u32>,
    /// kind "down" with rate_kbps: bytes per paced write.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub chunk_bytes: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AppActionKind {
    Down,
    Up,
}

impl Flow {
    /// The flow's plan-scoped id, regardless of type.
    pub fn flow_id(&self) -> u32 {
        match *self {
            Flow::Probe { flow_id, .. }
            | Flow::RealtimeUdp { flow_id, .. }
            | Flow::BulkTcp { flow_id, .. }
            | Flow::GatewayPing { flow_id, .. }
            | Flow::AppTraffic { flow_id, .. }
            | Flow::DscpProbe { flow_id, .. } => flow_id,
        }
    }
}

fn default_probe_packet_size() -> u32 {
    22
}
fn default_ping_interval_ms() -> u32 {
    1000
}
fn is_default_ping_interval(v: &u32) -> bool {
    *v == 1000
}
fn default_dscp_rate_pps() -> u32 {
    10
}
fn is_default_dscp_rate(v: &u32) -> bool {
    *v == 10
}

/// Server-allocated binding for one flow, keyed by flow_id. Lives in
/// RunTestPayload / header, not in the plan. `server_port` is absent for
/// gateway_ping flows (no server endpoint).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FlowBinding {
    pub flow_id: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_port: Option<u16>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Controller's generated client flattens the Flow union, so it sends
    /// the fields of every flow type (placeholders for the others). They must
    /// be ignored, not rejected.
    #[test]
    fn flow_ignores_fields_of_other_types() {
        let json = r#"{"type":"probe","flow_id":1,"rate_pps":50,"packet_size_bytes":22,
            "direction":"ToServer","dscp_values":[],"profile":"web","target_bytes":null}"#;
        let flow: Flow = serde_json::from_str(json).expect("extra fields are ignored");
        assert!(matches!(flow, Flow::Probe { .. }));
        // and they do not come back out (what the server stores and forwards)
        let out = serde_json::to_value(&flow).unwrap();
        assert!(out.get("profile").is_none() && out.get("dscp_values").is_none());
    }
}
