//! `contracts/schemas/capability.schema.json`

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// What a device CAN do. The server resolves the effective LoggingConfig as
/// `intersection(plan_profile, capability)` + overrides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Capability {
    pub radio: RadioCapability,
    pub qos: QosCapability,
    pub mobility: MobilityCapability,
    pub timestamp_sources: Vec<TimestampSource>,
}

/// Per-sample serving-cell fields a device can report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ServingCellField {
    Pci,
    Nci,
    Tac,
    Nrarfcn,
    Rsrp,
    Rsrq,
    Sinr,
}

/// Available clock sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TimestampSource {
    Mono,
    Utc,
    Gps,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RadioCapability {
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
pub struct QosCapability {
    /// 5G QoS Identifier availability.
    #[serde(rename = "5qi", default)]
    pub qi_5: bool,
    #[serde(default)]
    pub qfi: bool,
    #[serde(default)]
    pub slice_info: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MobilityCapability {
    #[serde(default)]
    pub explicit_ho_events: bool,
    #[serde(default)]
    pub rrc_state: bool,
    #[serde(default)]
    pub beam_switch: bool,
}
