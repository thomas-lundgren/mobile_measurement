//! `contracts/schemas/reachability_report.schema.json` — the device's own
//! check of which ports/protocols toward the server get through its current
//! network (wire protocol: `contracts/packets/reach.md`).

use crate::dto::device_status::DualClock;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Body of `POST /devices/{id}/reachability`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ReachabilityReport {
    pub checked_at: DualClock,
    pub trigger: ReachTrigger,
    pub network: NetworkContext,
    pub checks: Vec<ReachCheck>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReachTrigger {
    NetworkChange,
    ServiceStart,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum NetworkTransport {
    Cellular,
    Wifi,
    Ethernet,
    Other,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct NetworkContext {
    pub transport: NetworkTransport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wifi_ssid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wifi_freq_mhz: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validated: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ReachProtocol {
    Tcp,
    Udp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ReachKind {
    Reach,
    Http,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ReachCheck {
    pub protocol: ReachProtocol,
    pub port: u16,
    pub kind: ReachKind,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen_as: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
