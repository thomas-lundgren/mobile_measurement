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
    pub node: UploadNode,
    pub files: Vec<ManifestFile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ManifestFile {
    pub path: String,
    pub size_bytes: u64,
    /// Lowercase hex SHA-256 of the stored (post-encoding) bytes.
    pub sha256: String,
    pub encoding: FileEncoding,
}

/// The uploading node: only the mobile uploads (the server writes in place).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum UploadNode {
    Mobile,
}

/// How a file's bytes are encoded in the upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum FileEncoding {
    Identity,
    Zstd,
}

impl FileEncoding {
    pub fn as_str(self) -> &'static str {
        match self {
            FileEncoding::Identity => "identity",
            FileEncoding::Zstd => "zstd",
        }
    }
}

/// Response to phase 1: the resume mechanism (file-granular).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct UploadInitResponse {
    pub upload_id: String,
    pub needed_paths: Vec<String>,
}
