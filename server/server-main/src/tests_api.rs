//! Operator-facing test orchestration (controller surface, subset for 1i).
//! `POST /tests` drives: create session+test rows → test-control prepare →
//! wait for generators READY → enqueue RUN_TEST. `GET /tests/{id}` reports
//! status.

use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use mp_contracts::{
    Command, CreateTestRequest, DeviceSummary, InternalStartTest, LoggingConfig, LoggingConfigAck,
    RunTestPayload, TestControlState, TestResult, TestStatus, TestSummary,
};

use crate::auth::authed_operator;
use crate::commands;
use crate::error::AppError;
use crate::state::AppState;
use crate::tc_client::TcClient;
use crate::util::{new_id, now_utc_ns};

const PREPARE_TIMEOUT: Duration = Duration::from_secs(8);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/tests", post(create_test).get(list_tests))
        .route("/api/v1/tests/{test_id}", get(get_test))
        .route("/api/v1/tests/{test_id}/stop", post(stop_test))
        .route("/api/v1/devices", get(list_devices))
        .route(
            "/api/v1/devices/{device_id}/logging-config",
            post(set_device_logging_config),
        )
}

/// Operator pushes a logging-config update to a device: bump its version,
/// enqueue `SET_LOGGING_CONFIG`, and report the new version (the device applies
/// it and starts reporting it on its next heartbeat). Spec invariant 3.
#[utoipa::path(
    post, path = "/api/v1/devices/{device_id}/logging-config", tag = "operator",
    params(("device_id" = String, Path)),
    request_body = LoggingConfig,
    responses((status = 200, body = LoggingConfigAck), (status = 401), (status = 404))
)]
pub async fn set_device_logging_config(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
    Json(mut cfg): Json<LoggingConfig>,
) -> Result<Json<LoggingConfigAck>, AppError> {
    authed_operator(&st, &headers)?;
    let current: Option<i64> = sqlx::query_scalar(
        "SELECT logging_config_version FROM devices WHERE device_id = ? AND status = 'active'",
    )
    .bind(&device_id)
    .fetch_optional(&st.pool)
    .await?;
    let new_version = current.ok_or(AppError::NotFound)? + 1;
    cfg.version = new_version as u32; // server owns the version

    sqlx::query("UPDATE devices SET logging_config_version = ? WHERE device_id = ?")
        .bind(new_version)
        .bind(&device_id)
        .execute(&st.pool)
        .await?;
    let payload = serde_json::to_value(&cfg).map_err(|_| AppError::Internal)?;
    commands::enqueue(
        &st.pool,
        &device_id,
        Command::SetLoggingConfig,
        Some(&payload),
    )
    .await?;

    Ok(Json(LoggingConfigAck {
        device_id,
        logging_config_version: new_version,
    }))
}

#[utoipa::path(
    post, path = "/api/v1/tests", tag = "operator",
    request_body = CreateTestRequest,
    responses((status = 201, body = TestStatus), (status = 401), (status = 404), (status = 409))
)]
pub async fn create_test(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateTestRequest>,
) -> Result<(StatusCode, Json<TestStatus>), AppError> {
    authed_operator(&st, &headers)?;

    let tc_url = st
        .test_control_url
        .as_ref()
        .clone()
        .ok_or_else(|| AppError::BadRequest("test-control not configured".into()))?;
    let tc = TcClient::new(tc_url);

    if req.plan.flows.is_empty() {
        return Err(AppError::BadRequest("plan has no flows".into()));
    }

    // device must exist; grab its current config version
    let lcv: Option<i64> = sqlx::query_scalar(
        "SELECT logging_config_version FROM devices WHERE device_id = ? AND status = 'active'",
    )
    .bind(&req.device_id)
    .fetch_optional(&st.pool)
    .await?;
    let lcv = lcv.ok_or(AppError::NotFound)?;

    let test_id = new_id("tst");
    let session_id = new_id("ses");
    let now = now_utc_ns();
    let plan_json = serde_json::to_string(&req.plan).map_err(|_| AppError::Internal)?;
    let lc_json = match &req.logging_config_override {
        Some(c) => serde_json::to_string(c).map_err(|_| AppError::Internal)?,
        None => "{}".to_string(),
    };

    sqlx::query("INSERT INTO sessions(session_id, device_id, started_at) VALUES (?, ?, ?)")
        .bind(&session_id)
        .bind(&req.device_id)
        .bind(now)
        .execute(&st.pool)
        .await?;
    sqlx::query(
        "INSERT INTO tests(test_id, session_id, device_id, plan_name, plan_version, plan_json, \
         server_host, logging_config_json, logging_config_version, state, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'PREPARE', ?)",
    )
    .bind(&test_id)
    .bind(&session_id)
    .bind(&req.device_id)
    .bind(&req.plan.name)
    .bind(req.plan.version as i64)
    .bind(&plan_json)
    .bind(&st.server_host)
    .bind(&lc_json)
    .bind(lcv)
    .bind(now)
    .execute(&st.pool)
    .await?;

    // hand off to test-control and wait for generators to be ready
    tc.start_test(&InternalStartTest {
        test_id: test_id.clone(),
        device_id: req.device_id.clone(),
        session_id: session_id.clone(),
        server_host: st.server_host.clone(),
        plan: req.plan.clone(),
        logging_config_override: req.logging_config_override.clone(),
    })
    .await
    .map_err(|e| {
        tracing::error!("test-control start_test: {e}");
        AppError::Internal
    })?;

    let deadline = tokio::time::Instant::now() + PREPARE_TIMEOUT;
    let bindings = loop {
        let status = tc
            .test_status(&test_id)
            .await
            .map_err(|_| AppError::Internal)?;
        match status.state {
            TestControlState::WaitingForClient | TestControlState::Running => {
                break status.flow_bindings
            }
            TestControlState::Failed => {
                set_state(&st, &test_id, "FAILED", Some(TestResult::Failed)).await?;
                return Err(AppError::Conflict);
            }
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline {
            set_state(&st, &test_id, "FAILED", Some(TestResult::Failed)).await?;
            return Err(AppError::Internal);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    };

    // persist the bindings as flows, then enqueue RUN_TEST
    for b in &bindings {
        let flow = req.plan.flows.iter().find(|f| f.flow_id() == b.flow_id);
        let (ftype, direction) = flow_descr(flow);
        sqlx::query(
            "INSERT INTO flows(test_id, flow_id, type, direction, server_port) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&test_id)
        .bind(b.flow_id as i64)
        .bind(ftype)
        .bind(direction)
        .bind(b.server_port as i64)
        .execute(&st.pool)
        .await?;
    }

    let payload = RunTestPayload {
        test_id: test_id.clone(),
        session_id: session_id.clone(),
        server_host: st.server_host.clone(),
        test_plan: req.plan.clone(),
        flow_bindings: bindings,
        logging_config_override: req.logging_config_override.clone(),
    };
    let payload_val = serde_json::to_value(&payload).map_err(|_| AppError::Internal)?;
    commands::enqueue(
        &st.pool,
        &req.device_id,
        mp_contracts::Command::RunTest,
        Some(&payload_val),
    )
    .await?;

    set_state(&st, &test_id, "WAITING_FOR_CLIENT", None).await?;
    sqlx::query("UPDATE tests SET started_at = ? WHERE test_id = ?")
        .bind(now)
        .bind(&test_id)
        .execute(&st.pool)
        .await?;

    let status = read_status(&st, &test_id)
        .await?
        .ok_or(AppError::Internal)?;
    Ok((StatusCode::CREATED, Json(status)))
}

#[utoipa::path(
    get, path = "/api/v1/tests/{test_id}", tag = "operator",
    params(("test_id" = String, Path)),
    responses((status = 200, body = TestStatus), (status = 401), (status = 404))
)]
pub async fn get_test(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(test_id): Path<String>,
) -> Result<Json<TestStatus>, AppError> {
    authed_operator(&st, &headers)?;
    read_status(&st, &test_id)
        .await?
        .map(Json)
        .ok_or(AppError::NotFound)
}

/// Operator dashboard: every device with its latest reported status.
#[utoipa::path(
    get, path = "/api/v1/devices", tag = "operator",
    responses((status = 200, body = [DeviceSummary]), (status = 401))
)]
pub async fn list_devices(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<DeviceSummary>>, AppError> {
    authed_operator(&st, &headers)?;
    // devices LEFT JOIN their single latest status row.
    type Row = (
        String,
        Option<String>,
        String,
        Option<String>,
        String,
        i64,
        i64,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<i64>,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT d.device_id, d.device_label, d.platform, d.model, d.status, \
                d.logging_config_version, d.enrolled_at, \
                s.logger_state, s.current_test_id, s.battery_level_pct, s.received_at \
         FROM devices d LEFT JOIN device_status s ON s.device_id = d.device_id \
         ORDER BY d.created_at",
    )
    .fetch_all(&st.pool)
    .await?;
    let out = rows
        .into_iter()
        .map(|r| DeviceSummary {
            device_id: r.0,
            device_label: r.1,
            platform: r.2,
            model: r.3,
            status: r.4,
            logging_config_version: r.5,
            enrolled_at_utc_ns: r.6,
            logger_state: r.7,
            current_test_id: r.8,
            battery_level_pct: r.9,
            last_seen_utc_ns: r.10,
        })
        .collect();
    Ok(Json(out))
}

/// Operator recent-tests list (newest first, capped).
#[utoipa::path(
    get, path = "/api/v1/tests", tag = "operator",
    responses((status = 200, body = [TestSummary]), (status = 401))
)]
pub async fn list_tests(
    State(st): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<TestSummary>>, AppError> {
    authed_operator(&st, &headers)?;
    type Row = (
        String,
        String,
        String,
        String,
        Option<String>,
        i64,
        Option<i64>,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT test_id, device_id, plan_name, state, result, created_at, ended_at \
         FROM tests ORDER BY created_at DESC LIMIT 200",
    )
    .fetch_all(&st.pool)
    .await?;
    let out = rows
        .into_iter()
        .map(
            |(test_id, device_id, plan_name, state, result, created, ended)| {
                Ok(TestSummary {
                    test_id,
                    device_id,
                    plan_name,
                    state: serde_json::from_value(serde_json::Value::String(state))
                        .map_err(|_| AppError::Internal)?,
                    result: match result {
                        Some(r) => Some(
                            serde_json::from_value(serde_json::Value::String(r))
                                .map_err(|_| AppError::Internal)?,
                        ),
                        None => None,
                    },
                    created_utc_ns: created,
                    ended_utc_ns: ended,
                })
            },
        )
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(Json(out))
}

/// Operator stop: drive the test terminal (CANCELLED) and stop its generators.
/// Idempotent — stopping an already-terminal test just returns its status.
#[utoipa::path(
    post, path = "/api/v1/tests/{test_id}/stop", tag = "operator",
    params(("test_id" = String, Path)),
    responses((status = 200, body = TestStatus), (status = 401), (status = 404))
)]
pub async fn stop_test(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(test_id): Path<String>,
) -> Result<Json<TestStatus>, AppError> {
    authed_operator(&st, &headers)?;
    if read_status(&st, &test_id).await?.is_none() {
        return Err(AppError::NotFound);
    }
    let rows = sqlx::query(
        "UPDATE tests SET state = 'DONE', result = 'CANCELLED', ended_at = ? \
         WHERE test_id = ? AND state NOT IN ('DONE', 'FAILED')",
    )
    .bind(now_utc_ns())
    .bind(&test_id)
    .execute(&st.pool)
    .await?
    .rows_affected();
    if rows > 0 {
        if let Some(url) = st.test_control_url.as_ref() {
            if let Err(e) = TcClient::new(url.clone()).stop_test(&test_id).await {
                tracing::warn!("stop_test (operator) on {test_id}: {e}");
            }
        }
    }
    read_status(&st, &test_id)
        .await?
        .map(Json)
        .ok_or(AppError::NotFound)
}

fn flow_descr(flow: Option<&mp_contracts::Flow>) -> (&'static str, Option<&'static str>) {
    use mp_contracts::Flow;
    match flow {
        Some(Flow::Probe { .. }) => ("probe", None),
        Some(Flow::RealtimeUdp { direction, .. }) => ("realtime_udp", Some(dir_wire(*direction))),
        Some(Flow::BulkTcp { direction, .. }) => ("bulk_tcp", Some(dir_wire(*direction))),
        None => ("probe", None),
    }
}

fn dir_wire(d: mp_contracts::Direction) -> &'static str {
    match d {
        mp_contracts::Direction::FromServer => "FromServer",
        mp_contracts::Direction::ToServer => "ToServer",
    }
}

async fn set_state(
    st: &AppState,
    test_id: &str,
    state: &str,
    result: Option<TestResult>,
) -> Result<(), AppError> {
    sqlx::query("UPDATE tests SET state = ?, result = ? WHERE test_id = ?")
        .bind(state)
        .bind(result.map(|r| r.wire()))
        .bind(test_id)
        .execute(&st.pool)
        .await?;
    Ok(())
}

/// Advance a test to RUNNING when the device first reports it executing the test.
/// Guarded to only fire from WAITING_FOR_CLIENT, so a late status update can't
/// resurrect an already-terminal (DONE/FAILED) test.
pub async fn mark_running(pool: &sqlx::SqlitePool, test_id: &str) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE tests SET state = 'RUNNING' WHERE test_id = ? AND state = 'WAITING_FOR_CLIENT'",
    )
    .bind(test_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Finalize a test once its upload completes. The orchestration state lands on
/// DONE for a successful/partial run and FAILED otherwise; the precise outcome
/// is preserved in `result`, and `ended_at` is stamped. Guarded against
/// re-finalizing (idempotent re-posts of `complete`). Also tells test-control to
/// stop the test, which flushes the server node's Parquet writers and frees
/// ports — so the aggregated tree's `server/` files are complete for analysis.
pub async fn finalize_from_upload(
    st: &AppState,
    test_id: &str,
    result: TestResult,
) -> Result<(), AppError> {
    let state = match result {
        TestResult::Completed | TestResult::Partial => "DONE",
        TestResult::Failed | TestResult::Cancelled => "FAILED",
    };
    let rows = sqlx::query(
        "UPDATE tests SET state = ?, result = ?, ended_at = ? \
         WHERE test_id = ? AND state NOT IN ('DONE', 'FAILED')",
    )
    .bind(state)
    .bind(result.wire())
    .bind(now_utc_ns())
    .bind(test_id)
    .execute(&st.pool)
    .await?
    .rows_affected();

    // Only on the first finalize, and only if test-control is wired, stop the
    // generators (best-effort — a failure here must not fail the upload).
    if rows > 0 {
        if let Some(url) = st.test_control_url.as_ref() {
            if let Err(e) = TcClient::new(url.clone()).stop_test(test_id).await {
                tracing::warn!("stop_test on finalize: {e}");
            }
        }
    }
    Ok(())
}

/// (device_id, plan_name, state, result, created_at, started_at, ended_at)
type TestRow = (
    String,
    String,
    String,
    Option<String>,
    i64,
    Option<i64>,
    Option<i64>,
);

async fn read_status(st: &AppState, test_id: &str) -> Result<Option<TestStatus>, AppError> {
    let row: Option<TestRow> = sqlx::query_as(
        "SELECT device_id, plan_name, state, result, created_at, started_at, ended_at \
         FROM tests WHERE test_id = ?",
    )
    .bind(test_id)
    .fetch_optional(&st.pool)
    .await?;
    let Some((device_id, plan_name, state, result, created, started, ended)) = row else {
        return Ok(None);
    };
    let state: TestControlState =
        serde_json::from_value(serde_json::Value::String(state)).map_err(|_| AppError::Internal)?;
    let result = match result {
        Some(r) => Some(
            serde_json::from_value(serde_json::Value::String(r)).map_err(|_| AppError::Internal)?,
        ),
        None => None,
    };
    Ok(Some(TestStatus {
        test_id: test_id.to_string(),
        device_id,
        plan_name,
        state,
        result,
        created_utc_ns: created,
        started_utc_ns: started,
        ended_utc_ns: ended,
    }))
}
