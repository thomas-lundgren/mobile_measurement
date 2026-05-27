//! `contracts/schemas/logging_config.schema.json`

use super::capability::ServingCellField;
use super::monitor::{CaptureConfig, PingConfig};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// What a device WILL log. Server enforces `effective ⊆ capability`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LoggingConfig {
    /// Monotonic config version; drives SET_LOGGING_CONFIG drift detection.
    pub version: u32,
    pub radio: RadioLogging,
    pub qos: QosLogging,
    pub mobility: MobilityLogging,
    /// Gateable measurement events to log live (subset of EventType).
    pub events: Vec<GateableEvent>,
    pub gps: bool,
    pub clock_calibration_interval_sec: u32,
    /// Monitor only: VPN-based passive capture. Absent/null = disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<CaptureConfig>,
    /// Monitor only: background latency probing. Absent/null = disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ping: Option<PingConfig>,
}

/// The config-gateable subset of event types (operational events are always
/// logged regardless of config — see enums.yaml event_type_provenance).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
pub enum GateableEvent {
    #[serde(rename = "TEST_START")]
    TestStart,
    #[serde(rename = "TEST_STOP")]
    TestStop,
    #[serde(rename = "SIGNAL_LOSS_START")]
    SignalLossStart,
    #[serde(rename = "SIGNAL_LOSS_END")]
    SignalLossEnd,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RadioLogging {
    pub serving_cell: Vec<ServingCellField>,
    #[serde(default)]
    pub neighbor_cells: bool,
    #[serde(default)]
    pub lte_fallback: bool,
    #[serde(default)]
    pub cqi: bool,
    #[serde(default)]
    pub mcs: bool,
    #[serde(default)]
    pub bler: bool,
    #[serde(default)]
    pub harq: bool,
    #[serde(default)]
    pub drx: bool,
    #[serde(default)]
    pub bwp: bool,
    #[serde(default)]
    pub mimo_layers: bool,
    #[serde(default)]
    pub tx_power: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct QosLogging {
    #[serde(rename = "5qi", default)]
    pub qi_5: bool,
    #[serde(default)]
    pub qfi: bool,
    #[serde(default)]
    pub slice_info: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MobilityLogging {
    #[serde(default)]
    pub explicit_ho_events: bool,
    #[serde(default)]
    pub rrc_state: bool,
    #[serde(default)]
    pub beam_switch: bool,
}
