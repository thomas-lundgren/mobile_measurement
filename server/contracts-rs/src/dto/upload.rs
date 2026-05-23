//! Upload protocol DTOs (`contracts/upload/`).

use crate::enums::TestResult;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Body of `POST /tests/{id}/summary` (upload phase 1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct UploadManifest {
    pub test_result: TestResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_reason: Option<String>,
    /// Only the mobile uploads.
    pub node: String,
    pub files: Vec<ManifestFile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ManifestFile {
    pub path: String,
    pub size_bytes: u64,
    /// Lowercase hex SHA-256 of the stored (post-encoding) bytes.
    pub sha256: String,
    pub encoding: String,
}

/// Response to phase 1: the resume mechanism (file-granular).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct UploadInitResponse {
    pub upload_id: String,
    pub needed_paths: Vec<String>,
}
