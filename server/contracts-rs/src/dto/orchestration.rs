//! Orchestration DTOs: operator create-test, the RUN_TEST command payload, and
//! the controller test-status view.
//! (`contracts/schemas/{create_test_request,run_test_command}.schema.json`,
//! `planning/controller_openapi_sketch.yaml`)

use crate::dto::logging_config::LoggingConfig;
use crate::dto::test_plan::{FlowBinding, TestPlan};
use crate::enums::{TestControlState, TestResult};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

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
