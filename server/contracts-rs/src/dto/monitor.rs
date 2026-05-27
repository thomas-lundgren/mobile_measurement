//! MP Monitor DTOs — app inventory, monitor heartbeat state, capture/ping config.
//! Mirrors `contracts/schemas/app_list.schema.json` and the monitor sub-objects
//! in `capability.schema.json`, `device_status.schema.json`, and
//! `logging_config.schema.json`.

use crate::enums::VpnScope;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// ---------------------------------------------------------------------------
// App inventory (LIST_APPS response, pushed via POST /devices/{id}/apps)
// ---------------------------------------------------------------------------

/// Installed-application inventory reported by MP Monitor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AppList {
    pub device_id: String,
    /// Monotonically increments on every change; compared against
    /// `MonitorState::apps_inventory_version` in the heartbeat.
    pub inventory_version: u64,
    pub collected_utc_ns: i64,
    pub apps: Vec<AppInfo>,
}

/// One row in the installed-app inventory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AppInfo {
    /// Unique Android package name — the key for all per-app config.
    pub package_name: String,
    /// Human-readable label (display-only; may be ambiguous across packages).
    pub app_label: String,
    /// Android UID; used for VpnService exclusion and getConnectionOwnerUid.
    pub uid: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_name: Option<String>,
    /// True for system apps (FLAG_SYSTEM). Controller hides these by default.
    #[serde(default)]
    pub is_system: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_utc_ns: Option<i64>,
}

// ---------------------------------------------------------------------------
// Monitor capability (extends Capability for Monitor enrollments)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MonitorCapability {
    #[serde(default)]
    pub supports_vpn_capture: bool,
    #[serde(default)]
    pub supports_background_ping: bool,
    #[serde(default)]
    pub can_list_apps: bool,
}

// ---------------------------------------------------------------------------
// Heartbeat extensions (added to DeviceStatus for Monitor enrollments)
// ---------------------------------------------------------------------------

/// Per-interface default-route gateway state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct InterfaceLinkState {
    pub default_gw: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_mono_ns: Option<i64>,
}

/// Current link state per physical interface. Absent on Logger heartbeats.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LinkState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rmnet: Option<InterfaceLinkState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wlan: Option<InterfaceLinkState>,
}

/// Compact busy-peer row for the Controller's live view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TopPeer {
    pub peer_ip: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_label: Option<String>,
    /// Bytes (up+dn) observed in the last 60-second window.
    pub bytes_60s: u64,
}

/// Live snapshot of the Monitor's capture and ping engines. Absent on Logger heartbeats.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MonitorState {
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<VpnScope>,
    /// Current installed-app inventory version; server requests LIST_APPS when stale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apps_inventory_version: Option<u64>,
    #[serde(default)]
    pub bytes_observed: u64,
    #[serde(default)]
    pub bytes_today: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_remaining_mb: Option<u64>,
    #[serde(default)]
    pub buckets_pending: u32,
    #[serde(default)]
    pub pings_sent: u32,
    #[serde(default)]
    pub pings_lost: u32,
    #[serde(default)]
    pub top_peers: Vec<TopPeer>,
}

// ---------------------------------------------------------------------------
// logging_config extensions (capture + ping sub-objects)
// ---------------------------------------------------------------------------

/// VPN-based passive traffic capture config (Monitor only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CaptureConfig {
    pub enabled: bool,
    #[serde(default = "default_vpn_scope")]
    pub scope: VpnScope,
    /// Package names to capture when scope = Allowlist.
    #[serde(default)]
    pub apps: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_mb_per_day: Option<u32>,
}

/// Background latency-probing config (Monitor only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PingConfig {
    pub enabled: bool,
    /// Default probe cadence in ms.
    #[serde(default = "default_ping_interval")]
    pub default_interval_ms: u32,
    /// Operator-pinned targets. First-hop and auto-derived targets are added device-side.
    #[serde(default)]
    pub targets: Vec<PingTarget>,
    /// Package names whose observed peers feed the auto ping-target pool.
    #[serde(default)]
    pub follow_apps: Vec<String>,
    #[serde(default = "default_auto_target_count")]
    pub auto_target_count: u8,
}

fn default_ping_interval() -> u32 { 1000 }
fn default_auto_target_count() -> u8 { 5 }
fn default_vpn_scope() -> VpnScope { VpnScope::All }

/// A single operator-pinned latency target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PingTarget {
    pub label: String,
    /// IPv4, IPv6, or hostname. Resolved once on config apply.
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_ms: Option<u32>,
}

