//! Operator-facing test orchestration (controller surface, subset for 1i).
//! `POST /tests` drives: create session+test rows → test-control prepare →
//! wait for generators READY → enqueue RUN_TEST. `GET /tests/{id}` reports
//! status.

use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::{Json, Router};
use axum::routing::{get, post};
use mp_contracts::{
    CreateTestRequest, InternalStartTest, RunTestPayload, TestControlState, TestResult, TestStatus,
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
        .route("/api/v1/tests", post(create_test))
        .route("/api/v1/tests/{test_id}", get(get_test))
}

async fn create_test(
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
    let lcv: Option<i64> = sqlx::query_scalar("SELECT logging_config_version FROM devices WHERE device_id = ? AND status = 'active'")
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
        let status = tc.test_status(&test_id).await.map_err(|_| AppError::Internal)?;
        match status.state {
            TestControlState::WaitingForClient | TestControlState::Running => break status.flow_bindings,
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
        server_host: st.server_host.clone(),
        test_plan: req.plan.clone(),
        flow_bindings: bindings,
        logging_config_override: req.logging_config_override.clone(),
    };
    let payload_val = serde_json::to_value(&payload).map_err(|_| AppError::Internal)?;
    commands::enqueue(&st.pool, &req.device_id, mp_contracts::Command::RunTest, Some(&payload_val)).await?;

    set_state(&st, &test_id, "WAITING_FOR_CLIENT", None).await?;
    sqlx::query("UPDATE tests SET started_at = ? WHERE test_id = ?")
        .bind(now)
        .bind(&test_id)
        .execute(&st.pool)
        .await?;

    let status = read_status(&st, &test_id).await?.ok_or(AppError::Internal)?;
    Ok((StatusCode::CREATED, Json(status)))
}

async fn get_test(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(test_id): Path<String>,
) -> Result<Json<TestStatus>, AppError> {
    authed_operator(&st, &headers)?;
    read_status(&st, &test_id).await?.map(Json).ok_or(AppError::NotFound)
}

fn flow_descr(flow: Option<&mp_contracts::Flow>) -> (&'static str, Option<&'static str>) {
    use mp_contracts::{Direction, Flow};
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

async fn set_state(st: &AppState, test_id: &str, state: &str, result: Option<TestResult>) -> Result<(), AppError> {
    sqlx::query("UPDATE tests SET state = ?, result = ? WHERE test_id = ?")
        .bind(state)
        .bind(result.map(|r| r.wire()))
        .bind(test_id)
        .execute(&st.pool)
        .await?;
    Ok(())
}

async fn read_status(st: &AppState, test_id: &str) -> Result<Option<TestStatus>, AppError> {
    let row: Option<(String, String, String, Option<String>, i64, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT device_id, plan_name, state, result, created_at, started_at, ended_at \
         FROM tests WHERE test_id = ?",
    )
    .bind(test_id)
    .fetch_optional(&st.pool)
    .await?;
    let Some((device_id, plan_name, state, result, created, started, ended)) = row else {
        return Ok(None);
    };
    let state: TestControlState = serde_json::from_value(serde_json::Value::String(state)).map_err(|_| AppError::Internal)?;
    let result = match result {
        Some(r) => Some(serde_json::from_value(serde_json::Value::String(r)).map_err(|_| AppError::Internal)?),
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
