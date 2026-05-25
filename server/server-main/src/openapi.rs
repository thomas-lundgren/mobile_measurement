//! Code-first OpenAPI document. The authoritative `contracts/openapi/openapi.yaml`
//! is generated from this (see `tests/openapi_drift.rs`); CI fails on drift.
//! This replaces the `planning/*_openapi_sketch.yaml` design sketches.

use mp_contracts::*;
use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    info(title = "Measurement Platform API", version = "0.1.0"),
    paths(
        crate::auth::enroll,
        crate::auth::challenge,
        crate::auth::verify,
        crate::auth::login,
        crate::device::register,
        crate::device::poll_command,
        crate::device::ack_command,
        crate::device::post_status,
        crate::uploads::summary,
        crate::uploads::put_file,
        crate::uploads::complete,
        crate::tests_api::create_test,
        crate::tests_api::get_test,
        crate::tests_api::list_tests,
        crate::tests_api::stop_test,
        crate::tests_api::list_devices,
        crate::tests_api::set_device_logging_config,
    ),
    components(schemas(
        // auth
        EnrollRequest, HwInfo, EnrollResponse, ChallengeRequest, ChallengeResponse,
        VerifyRequest, VerifyResponse, LoginRequest, LoginResponse,
        // capability / logging config
        Capability, RadioCapability, QosCapability, MobilityCapability, ServingCellField,
        TimestampSource, LoggingConfig, RadioLogging, QosLogging, MobilityLogging, GateableEvent,
        // test plan / orchestration
        TestPlan, Flow, FlowBinding, CreateTestRequest, RunTestPayload, TestStatus,
        DeviceSummary, TestSummary, LoggingConfigAck,
        // device status / command
        DeviceStatus, DualClock, Battery, Storage, RadioSnapshot, DeviceCommand,
        // upload
        UploadManifest, ManifestFile, UploadInitResponse,
        // enums referenced above
        Command, Platform, OperatorRole, Direction, TestControlState, TestResult, LoggerState,
    ))
)]
pub struct ApiDoc;

/// The generated OpenAPI document. YAML serialization (the committed contract
/// form) is done by the caller — see `tests/openapi_drift.rs`.
pub fn document() -> utoipa::openapi::OpenApi {
    ApiDoc::openapi()
}
