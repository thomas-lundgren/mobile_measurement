//! Orchestration DTOs: operator create-test, the RUN_TEST command payload, and
//! the controller test-status view.
//! (`contracts/schemas/{create_test_request,run_test_command}.schema.json`,
//! `planning/controller_openapi_sketch.yaml`)

use crate::dto::logging_config::LoggingConfig;
use crate::dto::test_plan::{FlowBinding, TestPlan};
use crate::enums::{DeviceRole, TestControlState, TestResult};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Operator dashboard row: a device plus its latest reported status (status
/// fields are null until the device first heartbeats). `GET /api/v1/devices`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DeviceSummary {
    pub device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_label: Option<String>,
    pub platform: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub status: String,
    pub logging_config_version: i64,
    pub enrolled_at_utc_ns: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logger_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_test_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery_level_pct: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_utc_ns: Option<i64>,
    /// Enrolled app role. Absent on pre-Monitor rows — Controller treats as Logger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<DeviceRole>,
    /// Stable per-handset ID shared by Logger and Monitor APKs on the same device.
    /// Controller groups rows by this field to form the handset view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardware_id: Option<String>,
}

/// Result of pushing a logging-config update to a device
/// (`POST /api/v1/devices/{id}/logging-config`): the new effective version that
/// the device will report once it applies the `SET_LOGGING_CONFIG` command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LoggingConfigAck {
    pub device_id: String,
    pub logging_config_version: i64,
}

/// Compact test row for the operator's recent-tests list. `GET /api/v1/tests`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TestSummary {
    pub test_id: String,
    pub device_id: String,
    pub plan_name: String,
    pub state: TestControlState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<TestResult>,
    pub created_utc_ns: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_utc_ns: Option<i64>,
}

/// Operator → server-main body of `POST /api/v1/tests`. `plan` is a port-less
/// TestPlan; the server allocates ports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CreateTestRequest {
    pub device_id: String,
    pub plan: TestPlan,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logging_config_override: Option<LoggingConfig>,
}

/// Payload of the RUN_TEST command delivered to a device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct RunTestPayload {
    pub test_id: String,
    /// Server-assigned logging session this test belongs to; the device stamps
    /// it as the session_id join column.
    pub session_id: String,
    pub server_host: String,
    pub test_plan: TestPlan,
    pub flow_bindings: Vec<FlowBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logging_config_override: Option<LoggingConfig>,
}

/// Controller-facing test status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TestStatus {
    pub test_id: String,
    pub device_id: String,
    pub plan_name: String,
    pub state: TestControlState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<TestResult>,
    pub created_utc_ns: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_utc_ns: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_utc_ns: Option<i64>,
}
