//! Operator dashboard endpoints (Phase 3a): device list, test list, stop —
//! driven through the real router via tower oneshot, JWT-authed.

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use mp_contracts::OperatorRole;
use mp_server_main::{auth, build_router, db, AppState};
use serde_json::{json, Value};
use tower::ServiceExt;

async fn state() -> AppState {
    let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    let url = format!("sqlite://{}/mp.db", dir.path().display());
    AppState::new(
        db::connect(&url).await.unwrap(),
        "test-secret",
        "srv_test_01",
    )
}

async fn send(
    st: &AppState,
    method: &str,
    path: &str,
    bearer: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(b) = bearer {
        req = req.header("authorization", format!("Bearer {b}"));
    }
    let resp = build_router(st.clone())
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let val = if bytes.is_empty() {
        json!(null)
    } else {
        serde_json::from_slice(&bytes).unwrap_or(json!(null))
    };
    (status, val)
}

async fn operator_jwt(st: &AppState) -> String {
    auth::create_operator(&st.pool, "op", "pw123456", OperatorRole::Operator)
        .await
        .unwrap();
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/auth/login")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"username":"op","password":"pw123456"}).to_string(),
        ))
        .unwrap();
    let resp = build_router(st.clone()).oneshot(req).await.unwrap();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    v["jwt"].as_str().unwrap().to_string()
}

async fn seed_device(st: &AppState) {
    sqlx::query(
        "INSERT INTO devices(device_id, node_id, pubkey_spki, platform, model, \
            logging_config_version, status, enrolled_at, created_at) \
         VALUES('dev_a','node_a','pk','android','Pixel 9 Pro',1,'active',100,100)",
    )
    .execute(&st.pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn devices_requires_auth_and_lists_status() {
    let st = state().await;
    let jwt = operator_jwt(&st).await;

    // unauthenticated → 401
    let (s, _) = send(&st, "GET", "/api/v1/devices", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // empty fleet
    let (s, body) = send(&st, "GET", "/api/v1/devices", Some(&jwt)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 0);

    // a bare enrollment (no status yet) → status fields null
    seed_device(&st).await;
    let (_, body) = send(&st, "GET", "/api/v1/devices", Some(&jwt)).await;
    let d = &body.as_array().unwrap()[0];
    assert_eq!(d["device_id"], "dev_a");
    assert_eq!(d["model"], "Pixel 9 Pro");
    assert!(d.get("logger_state").is_none() || d["logger_state"].is_null());

    // after a heartbeat, liveness shows up
    sqlx::query(
        "INSERT INTO device_status(device_id, logger_state, logging_config_version, \
            battery_level_pct, sampled_mono_ns, sampled_utc_ns, received_at) \
         VALUES('dev_a','RUNNING_TEST',1,77,1,2,999)",
    )
    .execute(&st.pool)
    .await
    .unwrap();
    let (_, body) = send(&st, "GET", "/api/v1/devices", Some(&jwt)).await;
    let d = &body.as_array().unwrap()[0];
    assert_eq!(d["logger_state"], "RUNNING_TEST");
    assert_eq!(d["battery_level_pct"], 77);
    assert_eq!(d["last_seen_utc_ns"], 999);
}

#[tokio::test]
async fn tests_list_and_stop() {
    let st = state().await;
    let jwt = operator_jwt(&st).await;
    seed_device(&st).await;
    sqlx::query(
        "INSERT INTO sessions(session_id, device_id, started_at) VALUES('ses_a','dev_a',0)",
    )
    .execute(&st.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO tests(test_id, session_id, device_id, plan_name, plan_version, plan_json, \
            server_host, logging_config_json, logging_config_version, state, created_at) \
         VALUES('tst_a','ses_a','dev_a','p',1,'{}','h','{}',1,'RUNNING',5)",
    )
    .execute(&st.pool)
    .await
    .unwrap();

    // list
    let (s, body) = send(&st, "GET", "/api/v1/tests", Some(&jwt)).await;
    assert_eq!(s, StatusCode::OK);
    let row = &body.as_array().unwrap()[0];
    assert_eq!(row["test_id"], "tst_a");
    assert_eq!(row["state"], "RUNNING");

    // stop (test-control not configured in this AppState → state still flips; tc call skipped)
    let (s, body) = send(&st, "POST", "/api/v1/tests/tst_a/stop", Some(&jwt)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["state"], "DONE");
    assert_eq!(body["result"], "CANCELLED");
    assert!(body["ended_utc_ns"].as_i64().is_some());

    // stop unknown test → 404
    let (s, _) = send(&st, "POST", "/api/v1/tests/nope/stop", Some(&jwt)).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}
