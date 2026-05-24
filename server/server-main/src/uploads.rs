//! 3-phase resumable upload (`contracts/upload/`). Device-session guarded; the
//! test must belong to the authenticated device. Files are verified by size +
//! SHA-256 (over the stored bytes) and assembled under
//! `upload_root/{session_id}/{test_id}/{path}` — alongside the server's own
//! `server/` recordings for the same test.

use std::collections::HashSet;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{post, put};
use axum::{Json, Router};
use mp_contracts::{UploadInitResponse, UploadManifest};
use sha2::{Digest, Sha256};

use crate::auth::authed_device;
use crate::error::AppError;
use crate::state::AppState;
use crate::util::{new_id, now_utc_ns};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/tests/{test_id}/summary", post(summary))
        .route(
            "/api/v1/tests/{test_id}/upload/{upload_id}/files/{*path}",
            put(put_file),
        )
        .route(
            "/api/v1/tests/{test_id}/upload/{upload_id}/complete",
            post(complete),
        )
}

/// Authenticate the device and confirm it owns `test_id`; returns the test's
/// session_id (used to build the storage path).
async fn owned_test(
    st: &AppState,
    headers: &HeaderMap,
    test_id: &str,
) -> Result<(String, String), AppError> {
    let device_id = authed_device(st, headers).await?;
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT device_id, session_id FROM tests WHERE test_id = ?")
            .bind(test_id)
            .fetch_optional(&st.pool)
            .await?;
    match row {
        Some((owner, session_id)) if owner == device_id => Ok((device_id, session_id)),
        Some(_) => Err(AppError::Forbidden),
        None => Err(AppError::NotFound),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[utoipa::path(
    post, path = "/api/v1/tests/{test_id}/summary", tag = "upload",
    params(("test_id" = String, Path)),
    request_body = UploadManifest,
    responses((status = 200, body = UploadInitResponse), (status = 401), (status = 404))
)]
pub async fn summary(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(test_id): Path<String>,
    Json(manifest): Json<UploadManifest>,
) -> Result<Json<UploadInitResponse>, AppError> {
    owned_test(&st, &headers, &test_id).await?;
    if manifest.node != "mobile" {
        return Err(AppError::BadRequest("node must be 'mobile'".into()));
    }
    let manifest_json = serde_json::to_string(&manifest).map_err(|_| AppError::Internal)?;

    // Idempotent initiate: keep the same upload_id across re-posts.
    let existing: Option<(String,)> =
        sqlx::query_as("SELECT upload_id FROM uploads WHERE test_id = ? AND node = 'mobile'")
            .bind(&test_id)
            .fetch_optional(&st.pool)
            .await?;
    let upload_id = match existing {
        Some((id,)) => {
            sqlx::query(
                "UPDATE uploads SET manifest_json = ?, test_result = ?, \
                 state = CASE WHEN state = 'COMPLETE' THEN state ELSE 'IN_PROGRESS' END WHERE upload_id = ?",
            )
            .bind(&manifest_json)
            .bind(manifest.test_result.wire())
            .bind(&id)
            .execute(&st.pool)
            .await?;
            id
        }
        None => {
            let id = new_id("upl");
            sqlx::query(
                "INSERT INTO uploads(upload_id, test_id, node, state, test_result, manifest_json, created_at) \
                 VALUES (?, ?, 'mobile', 'IN_PROGRESS', ?, ?, ?)",
            )
            .bind(&id)
            .bind(&test_id)
            .bind(manifest.test_result.wire())
            .bind(&manifest_json)
            .bind(now_utc_ns())
            .execute(&st.pool)
            .await?;
            id
        }
    };

    let needed = missing_paths(&st, &test_id, &manifest).await?;
    Ok(Json(UploadInitResponse {
        upload_id,
        needed_paths: needed,
    }))
}

/// Manifest files not yet stored with a matching hash.
async fn missing_paths(
    st: &AppState,
    test_id: &str,
    manifest: &UploadManifest,
) -> Result<Vec<String>, AppError> {
    let received: Vec<(String, String)> = sqlx::query_as(
        "SELECT path, sha256 FROM upload_files WHERE test_id = ? AND node = 'mobile'",
    )
    .bind(test_id)
    .fetch_all(&st.pool)
    .await?;
    let have: HashSet<(String, String)> = received.into_iter().collect();
    Ok(manifest
        .files
        .iter()
        .filter(|f| !have.contains(&(f.path.clone(), f.sha256.clone())))
        .map(|f| f.path.clone())
        .collect())
}

async fn load_manifest(
    st: &AppState,
    upload_id: &str,
    test_id: &str,
) -> Result<UploadManifest, AppError> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT manifest_json FROM uploads WHERE upload_id = ? AND test_id = ?")
            .bind(upload_id)
            .bind(test_id)
            .fetch_optional(&st.pool)
            .await?;
    let (mj,) = row.ok_or(AppError::NotFound)?;
    serde_json::from_str(&mj).map_err(|_| AppError::Internal)
}

#[utoipa::path(
    put, path = "/api/v1/tests/{test_id}/upload/{upload_id}/files/{path}", tag = "upload",
    params(("test_id" = String, Path), ("upload_id" = String, Path), ("path" = String, Path)),
    request_body(content = String, content_type = "application/octet-stream"),
    responses((status = 204), (status = 422), (status = 401))
)]
pub async fn put_file(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path((test_id, upload_id, path)): Path<(String, String, String)>,
    body: Bytes,
) -> Result<StatusCode, AppError> {
    let (_device_id, session_id) = owned_test(&st, &headers, &test_id).await?;
    if path.contains("..") {
        return Err(AppError::BadRequest("invalid path".into()));
    }
    let manifest = load_manifest(&st, &upload_id, &test_id).await?;
    let file = manifest
        .files
        .iter()
        .find(|f| f.path == path)
        .ok_or_else(|| AppError::BadRequest("path not in manifest".into()))?;

    if body.len() as u64 != file.size_bytes {
        return Err(AppError::Unprocessable("size mismatch".into()));
    }
    if sha256_hex(&body) != file.sha256 {
        return Err(AppError::Unprocessable("sha256 mismatch".into()));
    }

    let dest = st.upload_root.join(&session_id).join(&test_id).join(&path);
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|_| AppError::Internal)?;
    }
    tokio::fs::write(&dest, &body)
        .await
        .map_err(|_| AppError::Internal)?;

    sqlx::query(
        "INSERT OR REPLACE INTO upload_files(test_id, node, path, sha256, size_bytes, encoding, received_at) \
         VALUES (?, 'mobile', ?, ?, ?, ?, ?)",
    )
    .bind(&test_id)
    .bind(&path)
    .bind(&file.sha256)
    .bind(file.size_bytes as i64)
    .bind(&file.encoding)
    .bind(now_utc_ns())
    .execute(&st.pool)
    .await?;

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post, path = "/api/v1/tests/{test_id}/upload/{upload_id}/complete", tag = "upload",
    params(("test_id" = String, Path), ("upload_id" = String, Path)),
    responses((status = 204), (status = 409), (status = 401))
)]
pub async fn complete(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path((test_id, upload_id)): Path<(String, String)>,
) -> Result<StatusCode, AppError> {
    owned_test(&st, &headers, &test_id).await?;
    let manifest = load_manifest(&st, &upload_id, &test_id).await?;

    // every manifest file must be present with a matching hash
    if !missing_paths(&st, &test_id, &manifest).await?.is_empty() {
        return Err(AppError::Conflict);
    }
    sqlx::query("UPDATE uploads SET state = 'COMPLETE', completed_at = ? WHERE upload_id = ?")
        .bind(now_utc_ns())
        .bind(&upload_id)
        .execute(&st.pool)
        .await?;

    // Upload is the terminal signal: finalize the test (state + result + ended_at)
    // and stop the server-side generators so their server/ Parquet is flushed.
    crate::tests_api::finalize_from_upload(&st, &test_id, manifest.test_result).await?;
    Ok(StatusCode::NO_CONTENT)
}
