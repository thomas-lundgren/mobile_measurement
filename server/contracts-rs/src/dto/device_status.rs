//! `contracts/schemas/device_status.schema.json`

use crate::dto::monitor::{LinkState, MonitorState};
use crate::enums::LoggerState;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Body of `POST /devices/{id}/status` — the periodic device heartbeat.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DeviceStatus {
    pub logger_state: LoggerState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_test_id: Option<String>,
    pub logging_config_version: u32,
    pub sampled_at: DualClock,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery: Option<Battery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<Storage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_uploads: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radio: Option<RadioSnapshot>,
    /// Monitor only: current default-route gateway per interface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_state: Option<LinkState>,
    /// Monitor only: live capture/ping engine snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor_state: Option<MonitorState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DualClock {
    pub mono_ns: i64,
    pub utc_ns: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Battery {
    pub level_pct: u8,
    pub charging: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Storage {
    pub free_bytes: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<i64>,
}

/// Coarse serving-cell snapshot for the dashboard — NOT a measurement record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RadioSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pci: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nci: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rsrp: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rsrq: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sinr: Option<i32>,
}

/// Response of `GET /api/v1/time`: the server's clock, for the UE's
/// HTTP clock-sync fallback (SNTP-over-HTTP; spec §11.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ServerTime {
    /// Server UTC when the request was handled.
    pub server_utc_ns: i64,
    /// UDP time responder the UE should prefer (contracts/packets/time_sync.md).
    pub time_udp_port: u16,
}
