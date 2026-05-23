//! Device control-plane endpoints (session-authenticated): register,
//! command poll/ack, status. Identity is always the token's device; a path
//! `{device_id}` that disagrees is rejected.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use mp_contracts::{Capability, Command, DeviceCommand, DeviceStatus, LoggingConfig};

use crate::auth::authed_device;
use crate::error::AppError;
use crate::resolve::resolve_effective_config;
use crate::state::AppState;
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
    if commands::ack(&st.pool, &device_id, command_id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound)
    }
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
    Ok(StatusCode::NO_CONTENT)
}
