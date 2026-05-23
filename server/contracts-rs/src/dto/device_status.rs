//! `contracts/schemas/device_status.schema.json`

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
