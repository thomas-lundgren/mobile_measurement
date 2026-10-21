//! 3-phase resumable upload (`contracts/upload/`). Device-session guarded; the
//! test must belong to the authenticated device. Files are verified by size +
//! SHA-256 (over the stored bytes) and assembled under
//! `upload_root/{session_id}/{test_id}/{path}` — alongside the server's own
//! `server/` recordings for the same test.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};

use axum::body::Bytes;
use axum::extract::{ConnectInfo, FromRequestParts, Path, State};
use axum::http::request::Parts;
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

/// The TCP peer's address, when the server runs with connect info (absent in
/// router-level tests that don't set it).
pub struct Peer(pub Option<IpAddr>);

impl<S: Send + Sync> FromRequestParts<S> for Peer {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Peer(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|c| c.0.ip()),
        ))
    }
}

/// Local network: private, loopback or link-local ranges, or the server's own
/// public address (the router's hairpin NAT when the phone is on home Wi-Fi).
/// Carrier-grade NAT (100.64/10) counts as the mobile network.
pub(crate) fn is_local(ip: IpAddr, server_host: &str) -> bool {
    if server_host.parse::<IpAddr>().is_ok_and(|h| h == ip) {
        return true;
    }
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_local(IpAddr::V4(v4), server_host);
            }
            v6.is_loopback()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Why an upload from `peer` is held, or None to accept it. With a hold
/// configured, uploads wait for the local network unless the device's last
/// heartbeat reported storage running low. A held upload gets 503: the
/// Logger keeps the files and retries with backoff.
async fn upload_held(
    st: &AppState,
    device_id: &str,
    peer: Option<IpAddr>,
) -> Result<Option<String>, AppError> {
    let Some(hold) = st.upload_hold else {
        return Ok(None);
    };
    if peer.is_some_and(|ip| is_local(ip, &st.server_host)) {
        return Ok(None);
    }
    let free: Option<(Option<i64>,)> =
        sqlx::query_as("SELECT storage_free_bytes FROM device_status WHERE device_id = ?")
            .bind(device_id)
            .fetch_optional(&st.pool)
            .await?;
    if let Some((Some(free),)) = free {
        if free < hold.min_free_bytes {
            return Ok(None);
        }
    }
    let from = peer.map_or("an unknown address".to_string(), |ip| ip.to_string());
    Ok(Some(format!(
        "upload held until the device is on the local network (from {from})"
    )))
}

#[utoipa::path(
    post, path = "/api/v1/tests/{test_id}/summary", tag = "upload",
    params(("test_id" = String, Path)),
    request_body = UploadManifest,
    responses(
        (status = 200, body = UploadInitResponse), (status = 401), (status = 404),
        (status = 503, description = "Upload held (device not on the local network) and every base file is already stored; retry after a network change")
    )
)]
pub async fn summary(
    State(st): State<AppState>,
    Peer(peer): Peer,
    headers: HeaderMap,
    Path(test_id): Path<String>,
    Json(manifest): Json<UploadManifest>,
) -> Result<Json<UploadInitResponse>, AppError> {
    let (device_id, _) = owned_test(&st, &headers, &test_id).await?;
    let held = upload_held(&st, &device_id, peer).await?;
    if let Some(bad) = manifest.files.iter().find(|f| !valid_upload_path(&f.path)) {
        return Err(AppError::BadRequest(format!(
            "not an upload path: {:?}",
            bad.path
        )));
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

    let mut needed = missing_paths(&st, &test_id, &manifest).await?;
    if let Some(why) = held {
        // Away from home: take the small base files (radio, GPS, …) so every
        // test shows where and on what it ran at once; the rest waits.
        needed.retain(|p| is_base_path(p));
        if needed.is_empty() {
            return Err(AppError::Unavailable(why));
        }
    }
    Ok(Json(UploadInitResponse {
        upload_id,
        needed_paths: needed,
    }))
}

/// The files a held upload still takes (contracts/upload/README.md "Held
/// uploads"; vectors: testvectors/upload/paths.json `base`): the test's
/// header, GPS, radio, clock and events — tens of kB, versus megabytes of
/// packet logs.
pub fn is_base_path(path: &str) -> bool {
    if !valid_upload_path(path) {
        return false;
    }
    let name = path.strip_prefix("mobile/").unwrap_or(path);
    let base_name = !path.starts_with("mobile/");
    path == "header.json"
        || (base_name && (name.starts_with("gps") || name.starts_with("radio")))
        || (!base_name && (name.starts_with("clock") || name.starts_with("events")))
}

/// A path the mobile may upload, test-relative — the `files[].path` pattern
/// of contracts/schemas/upload_manifest.schema.json (vectors:
/// testvectors/upload/paths.json). Anything else (absolute, `..`, another
/// node's folder) could land outside the test's folder, so it is refused.
pub fn valid_upload_path(path: &str) -> bool {
    // <stem>.parquet or <stem>_NNNN.parquet (4+ digits)
    fn table(name: &str, stems: &[&str]) -> bool {
        let Some(base) = name.strip_suffix(".parquet") else {
            return false;
        };
        stems.iter().any(|stem| match base.strip_prefix(stem) {
            Some("") => true,
            Some(rest) => rest
                .strip_prefix('_')
                .is_some_and(|n| n.len() >= 4 && n.bytes().all(|b| b.is_ascii_digit())),
            None => false,
        })
    }
    match path.split_once('/') {
        None => path == "header.json" || table(path, &["gps", "radio"]),
        Some(("mobile", name)) => {
            name == "probe.jsonl.gz" || table(name, &["packets", "events", "clock", "pings"])
        }
        Some(_) => false,
    }
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
    if !valid_upload_path(&path) {
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
    .bind(file.encoding.as_str())
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

#[cfg(test)]
mod tests {
    use super::{is_base_path, valid_upload_path};

    #[test]
    fn base_paths_follow_the_contract_vectors() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/testvectors/upload/paths.json"
        );
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let base: Vec<&str> = doc["base"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_str().unwrap())
            .collect();
        for key in ["valid", "invalid"] {
            for p in doc[key].as_array().unwrap() {
                let p = p.as_str().unwrap();
                assert_eq!(is_base_path(p), base.contains(&p), "{p:?}");
            }
        }
    }

    #[test]
    fn upload_paths_follow_the_contract_vectors() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/testvectors/upload/paths.json"
        );
        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        for (want, key) in [(true, "valid"), (false, "invalid")] {
            for p in doc[key].as_array().unwrap() {
                let p = p.as_str().unwrap();
                assert_eq!(valid_upload_path(p), want, "{p:?}");
            }
        }
    }
}
