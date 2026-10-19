//! Device control-plane endpoints (session-authenticated): register,
//! command poll/ack, status, reachability reports. Identity is always the token's device; a path
//! `{device_id}` that disagrees is rejected.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use mp_contracts::{
    Capability, Command, DeviceCommand, DeviceStatus, LoggingConfig, ReachabilityReport,
};

use crate::auth::authed_device;
use crate::error::AppError;
use crate::resolve::resolve_effective_config;
use crate::state::AppState;
use crate::tc_client::TcClient;
use crate::util::now_utc_ns;
use crate::{commands, registry};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/devices/register", post(register))
        .route("/api/v1/devices/{device_id}/command", get(poll_command))
        .route(
            "/api/v1/devices/{device_id}/command/{command_id}/ack",
            post(ack_command),
        )
        .route("/api/v1/devices/{device_id}/status", post(post_status))
        .route(
            "/api/v1/devices/{device_id}/reachability",
            post(post_reachability),
        )
        .route("/api/v1/time", get(server_time))
}

/// Unauthenticated server-clock reading for device-side time sync. The UE pairs
/// its own send/recv timestamps around this call (SNTP-over-HTTP) to estimate
/// its offset from the server clock, which it records in `clock.parquet` so OWD
/// can be offset-corrected (invariant 2; spec §11.3). `time_udp_port` advertises
/// the lower-overhead UDP responder, which the UE prefers for a tighter bound.
pub async fn server_time(State(st): State<AppState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "server_utc_ns": now_utc_ns(),
        "time_udp_port": st.time_udp_port,
    }))
}

/// Authenticate, and if the route carries a `{device_id}`, require it to match.
async fn authed_for(
    st: &AppState,
    headers: &HeaderMap,
    path_device: Option<&str>,
) -> Result<String, AppError> {
    let device_id = authed_device(st, headers).await?;
    if let Some(p) = path_device {
        if p != device_id {
            return Err(AppError::Forbidden);
        }
    }
    Ok(device_id)
}

#[utoipa::path(
    post, path = "/api/v1/devices/register", tag = "device",
    request_body = Capability,
    responses((status = 200, body = LoggingConfig), (status = 401))
)]
pub async fn register(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(cap): Json<Capability>,
) -> Result<Json<LoggingConfig>, AppError> {
    let device_id = authed_for(&st, &headers, None).await?;

    let cap_json = serde_json::to_string(&cap).map_err(|_| AppError::Internal)?;
    sqlx::query("UPDATE devices SET capability_json = ? WHERE device_id = ?")
        .bind(&cap_json)
        .bind(&device_id)
        .execute(&st.pool)
        .await?;

    let version: i64 =
        sqlx::query_scalar("SELECT logging_config_version FROM devices WHERE device_id = ?")
            .bind(&device_id)
            .fetch_one(&st.pool)
            .await?;

    Ok(Json(resolve_effective_config(&cap, version as u32)))
}

#[utoipa::path(
    get, path = "/api/v1/devices/{device_id}/command", tag = "device",
    params(("device_id" = String, Path)),
    responses((status = 200, body = DeviceCommand), (status = 204), (status = 401))
)]
pub async fn poll_command(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
) -> Result<Response, AppError> {
    let device_id = authed_for(&st, &headers, Some(&device_id)).await?;

    match commands::next_pending(&st.pool, &device_id).await? {
        None => Ok(StatusCode::NO_CONTENT.into_response()),
        Some((command_id, wire, payload_json)) => {
            let command: Command = serde_json::from_value(serde_json::Value::String(wire))
                .map_err(|_| AppError::Internal)?;
            let payload = match payload_json {
                Some(s) => serde_json::from_str(&s).map_err(|_| AppError::Internal)?,
                None => serde_json::Value::Null,
            };
            Ok(Json(DeviceCommand {
                command_id,
                command,
                payload,
            })
            .into_response())
        }
    }
}

#[utoipa::path(
    post, path = "/api/v1/devices/{device_id}/command/{command_id}/ack", tag = "device",
    params(("device_id" = String, Path), ("command_id" = i64, Path)),
    responses((status = 204), (status = 404), (status = 401))
)]
pub async fn ack_command(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path((device_id, command_id)): Path<(String, i64)>,
) -> Result<StatusCode, AppError> {
    let device_id = authed_for(&st, &headers, Some(&device_id)).await?;
    if !commands::ack(&st.pool, &device_id, command_id).await? {
        return Err(AppError::NotFound);
    }
    // A RUN_TEST ack means the UE has finished measuring and queued its upload
    // locally. Stop the server-side generators now (flushing server/ Parquet)
    // rather than at upload-complete, which may be hours later when uploads
    // are deferred through a coverage gap. Best-effort: never fails the ack.
    if let Some(test_id) = commands::run_test_id(&st.pool, &device_id, command_id).await? {
        if let Some(url) = st.test_control_url.as_ref() {
            if let Err(e) = TcClient::new(url.clone()).stop_test(&test_id).await {
                tracing::warn!("stop_test on RUN_TEST ack ({test_id}): {e}");
            }
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The device's own reachability check (which ports/protocols toward the
/// server get through the network it was on). Checks run on the device
/// without the server — on a restrictive network it cannot be reached — and
/// are uploaded later; each report is appended as one JSON line to
/// `<upload_root>/reach/<device_id>.jsonl`.
#[utoipa::path(
    post, path = "/api/v1/devices/{device_id}/reachability", tag = "device",
    params(("device_id" = String, Path)),
    request_body = ReachabilityReport,
    responses((status = 204), (status = 401))
)]
pub async fn post_reachability(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
    Json(report): Json<ReachabilityReport>,
) -> Result<StatusCode, AppError> {
    let device_id = authed_for(&st, &headers, Some(&device_id)).await?;
    let mut line = serde_json::to_value(&report).map_err(|_| AppError::Internal)?;
    line["device_id"] = device_id.clone().into();
    line["received_at_utc_ns"] = now_utc_ns().into();
    let dir = st.upload_root.join("reach");
    let path = dir.join(format!("{device_id}.jsonl"));
    let text = format!("{line}\n");
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        use std::io::Write;
        std::fs::create_dir_all(&dir)?;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?
            .write_all(text.as_bytes())
    })
    .await
    .map_err(|_| AppError::Internal)?
    .map_err(|_| AppError::Internal)?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post, path = "/api/v1/devices/{device_id}/status", tag = "device",
    params(("device_id" = String, Path)),
    request_body = DeviceStatus,
    responses((status = 204), (status = 401))
)]
pub async fn post_status(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
    Json(status): Json<DeviceStatus>,
) -> Result<StatusCode, AppError> {
    let device_id = authed_for(&st, &headers, Some(&device_id)).await?;

    let radio_json = match &status.radio {
        Some(r) => Some(serde_json::to_string(r).map_err(|_| AppError::Internal)?),
        None => None,
    };
    let update = registry::StatusUpdate {
        device_id,
        logger_state: status.logger_state.wire().to_string(),
        current_test_id: status.current_test_id.clone(),
        logging_config_version: status.logging_config_version as i64,
        battery_level_pct: status.battery.map(|b| b.level_pct as i64),
        battery_charging: status.battery.map(|b| b.charging),
        storage_free_bytes: status.storage.map(|s| s.free_bytes),
        pending_uploads: status.pending_uploads.map(|p| p as i64),
        radio_json,
        sampled_mono_ns: status.sampled_at.mono_ns,
        sampled_utc_ns: status.sampled_at.utc_ns,
        received_at: now_utc_ns(),
    };
    registry::upsert_device_status(&st.pool, &update).await?;

    // Advance the test's orchestration state the first time the device reports
    // it actively running that test (WAITING_FOR_CLIENT -> RUNNING).
    if matches!(status.logger_state, mp_contracts::LoggerState::RunningTest) {
        if let Some(test_id) = &status.current_test_id {
            crate::tests_api::mark_running(&st.pool, test_id).await?;
        }
    }
    Ok(StatusCode::NO_CONTENT)
}
