//! Command-poll response (the device's `GET /devices/{id}/command`).

use crate::enums::Command;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// One queued command delivered to a device. `payload` shape depends on
/// `command` (e.g. RunTestPayload for RUN_TEST); the device acks by
/// `command_id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DeviceCommand {
    pub command_id: i64,
    pub command: Command,
    #[schema(value_type = Object)]
    #[serde(default)]
    pub payload: serde_json::Value,
}
