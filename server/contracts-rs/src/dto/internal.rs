//! Internal control-IPC DTOs (`contracts/internal/`, loopback HTTP+JSON).
//! 1f covers the test-control → data-generator hop; the server-main →
//! test-control hop DTOs are added in 1g.

use crate::dto::logging_config::LoggingConfig;
use crate::dto::test_plan::{Flow, FlowBinding, TestPlan};
use crate::enums::{GeneratorState, TestControlState, TestResult};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// `POST /internal/tests` (server-main → test-control). server-main has
/// assigned the ids and created the session/test rows; test-control allocates
/// ports and coordinates generators.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct InternalStartTest {
    pub test_id: String,
    pub device_id: String,
    pub session_id: String,
    pub server_host: String,
    pub plan: TestPlan,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logging_config_override: Option<LoggingConfig>,
}

/// `GET /internal/tests/{id}` (test-control → server-main).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct InternalTestStatus {
    pub test_id: String,
    pub device_id: String,
    pub state: TestControlState,
    #[serde(default)]
    pub flow_bindings: Vec<FlowBinding>,
    pub generators: Vec<GeneratorRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<TestResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Per-flow generator coordination state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct GeneratorRef {
    pub flow_id: u32,
    pub generator_id: String,
    pub state: GeneratorState,
}

/// `POST /internal/generators` (test-control → data-generator). Configures one
/// generator for one flow. device_id/session_id are carried so the server-side
/// packets.parquet gets the same join columns as the mobile's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct InternalGeneratorPrepare {
    pub test_id: String,
    pub device_id: String,
    pub session_id: String,
    pub flow: Flow,
    pub server_port: u16,
    /// Filesystem path for this node's `server/` log subtree for the test.
    pub node_log_dir: String,
}

/// `GET /internal/generators/{id}` and the `.../stop` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct InternalGeneratorStatus {
    pub generator_id: String,
    pub test_id: String,
    pub flow_id: u32,
    pub state: GeneratorState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_port: Option<u16>,
    pub counters: GeneratorCounters,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GeneratorCounters {
    pub packets_sent: u64,
    pub packets_received: u64,
    #[serde(default)]
    pub bytes_sent: u64,
    #[serde(default)]
    pub bytes_received: u64,
    /// Logger-sink rows dropped to ring overflow; >0 marks the test PARTIAL.
    pub dropped_rows: u64,
}

/// The internal control API's types as an OpenAPI document (components only:
/// the routes are loopback-only and not part of the public API). The
/// committed `contracts/openapi/internal.yaml` is generated from it
/// (`tests/internal_openapi_drift.rs`), and `ci/validate_contracts.py`
/// compares the `internal_*` JSON Schemas with it.
#[derive(utoipa::OpenApi)]
#[openapi(
    info(title = "Measurement Platform internal control API", version = "0.1.0"),
    components(schemas(
        InternalStartTest,
        InternalTestStatus,
        InternalGeneratorPrepare,
        InternalGeneratorStatus
    ))
)]
pub struct InternalApiDoc;
