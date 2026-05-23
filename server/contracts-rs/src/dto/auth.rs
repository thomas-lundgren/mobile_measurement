//! Auth DTOs — `planning/auth_openapi_sketch.yaml` (device + operator).

use crate::enums::{OperatorRole, Platform};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Body of `POST /devices/enroll` (Bearer = one-time enrollment token).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct EnrollRequest {
    /// SPKI DER, base64.
    pub device_pubkey: String,
    pub pubkey_format: String,
    pub sig_alg: String,
    pub sig_format: String,
    pub platform: Platform,
    #[serde(default)]
    pub hw_info: HwInfo,
    pub protocol_version: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct HwInfo {
    pub model: Option<String>,
    pub manufacturer: Option<String>,
    pub os_version: Option<String>,
    pub device_label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct EnrollResponse {
    pub device_id: String,
    pub node_id: String,
    pub server_time_utc_ns: i64,
    pub schema_version: u32,
    pub logging_config_version: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ChallengeRequest {
    pub device_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ChallengeResponse {
    pub challenge_id: String,
    /// base64, 32 random bytes.
    pub nonce: String,
    pub server_id: String,
    pub expires_at_utc_ns: i64,
    pub server_time_utc_ns: i64,
    pub auth_domain: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct VerifyRequest {
    pub challenge_id: String,
    /// base64 ASN.1-DER signature over the mp-auth-v1 blob.
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct VerifyResponse {
    pub session_token: String,
    pub expires_at_utc_ns: i64,
    pub logging_config_version: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LoginResponse {
    pub jwt: String,
    pub expires_at_utc_ns: i64,
    pub role: OperatorRole,
}
