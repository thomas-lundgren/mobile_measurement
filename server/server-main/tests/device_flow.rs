//! Device control-plane integration tests: register, command poll/ack, status,
//! and the auth guard — driven through the real router via tower oneshot.

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use fieldtest_core::auth as core_auth;
use mp_contracts::*;
use mp_server_main::{build_router, commands, db, AppState};
use p256::ecdsa::SigningKey;
use serde_json::json;
use tower::ServiceExt;

async fn state() -> AppState {
    let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    let url = format!("sqlite://{}/mp.db", dir.path().display());
    AppState::new(db::connect(&url).await.unwrap(), "test-secret", "srv_test_01")
}

async fn send(state: &AppState, method: &str, path: &str, bearer: Option<&str>, body: Option<serde_json::Value>) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(b) = bearer {
        req = req.header("authorization", format!("Bearer {b}"));
    }
    let req = match body {
        Some(v) => req.header("content-type", "application/json").body(Body::from(v.to_string())).unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let resp = build_router(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let val = if bytes.is_empty() { json!(null) } else { serde_json::from_slice(&bytes).unwrap() };
    (status, val)
}

/// Enroll + authenticate a device; returns (device_id, session_token).
async fn enrolled_session(st: &AppState) -> (String, String) {
    let sk: SigningKey = {
        let secret: [u8; 32] = std::array::from_fn(|i| (i + 11) as u8);
        SigningKey::from_bytes(&secret.into()).unwrap()
    };
    mp_server_main::registry::mint_enrollment_token(&st.pool, &mp_server_main::util::hash_token("tok"), 0, i64::MAX).await.unwrap();
    let (_, b) = send(st, "POST", "/api/v1/devices/enroll", Some("tok"), Some(json!({
        "device_pubkey": core_auth::public_key_spki_b64(&sk),
        "pubkey_format": AUTH_PUBKEY_FORMAT, "sig_alg": AUTH_SIGNING_ALG, "sig_format": AUTH_SIG_FORMAT,
        "platform": "android", "hw_info": {}, "protocol_version": PROTOCOL_VERSION
    }))).await;
    let device_id = b["device_id"].as_str().unwrap().to_string();

    let (_, b) = send(st, "POST", "/api/v1/auth/challenge", None, Some(json!({"device_id": device_id}))).await;
    let (cid, nonce) = (b["challenge_id"].as_str().unwrap().to_string(), b["nonce"].as_str().unwrap().to_string());
    let sig = core_auth::sign_der_b64(&sk, &core_auth::build_message(AUTH_DOMAIN, "srv_test_01", &cid, &nonce));
    let (_, b) = send(st, "POST", "/api/v1/auth/verify", None, Some(json!({"challenge_id": cid, "signature": sig}))).await;
    (device_id, b["session_token"].as_str().unwrap().to_string())
}

fn pixel_capability() -> serde_json::Value {
    json!({
        "radio": { "serving_cell": ["pci","nci","rsrp","rsrq","sinr"], "neighbor_cells": true, "lte_fallback": true,
                   "cqi": false, "mcs": false, "bler": false, "harq": false, "drx": false, "bwp": false,
                   "mimo_layers": false, "tx_power": false },
        "qos": { "5qi": false, "qfi": false, "slice_info": false },
        "mobility": { "explicit_ho_events": false, "rrc_state": false, "beam_switch": false },
        "timestamp_sources": ["mono","utc","gps"]
    })
}

#[tokio::test]
async fn register_returns_effective_subset_of_capability() {
    let st = state().await;
    let (_dev, session) = enrolled_session(&st).await;

    let (s, cfg) = send(&st, "POST", "/api/v1/devices/register", Some(&session), Some(pixel_capability())).await;
    assert_eq!(s, StatusCode::OK, "register: {cfg}");
    // serving_cell ⊆ capability (tac/nrarfcn not reported → absent)
    let sc = cfg["radio"]["serving_cell"].as_array().unwrap();
    assert!(sc.iter().any(|v| v == "pci"));
    assert!(!sc.iter().any(|v| v == "tac"));
    assert_eq!(cfg["radio"]["cqi"], false);
    assert_eq!(cfg["gps"], true);
    assert_eq!(cfg["version"], 1);
}

#[tokio::test]
async fn endpoints_require_session() {
    let st = state().await;
    let (s, _) = send(&st, "POST", "/api/v1/devices/register", None, Some(pixel_capability())).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = send(&st, "POST", "/api/v1/devices/register", Some("garbage-token"), Some(pixel_capability())).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn command_enqueue_poll_ack_cycle() {
    let st = state().await;
    let (device_id, session) = enrolled_session(&st).await;

    // nothing queued → 204
    let path = format!("/api/v1/devices/{device_id}/command");
    let (s, _) = send(&st, "GET", &path, Some(&session), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // enqueue REQUEST_STATUS, then poll → 200 with the command
    commands::enqueue(&st.pool, &device_id, Command::RequestStatus, None).await.unwrap();
    let (s, body) = send(&st, "GET", &path, Some(&session), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["command"], "REQUEST_STATUS");
    let cmd_id = body["command_id"].as_i64().unwrap();

    // a second poll → 204 (already delivered)
    let (s, _) = send(&st, "GET", &path, Some(&session), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // ack → 204; re-ack → 404
    let ack = format!("/api/v1/devices/{device_id}/command/{cmd_id}/ack");
    let (s, _) = send(&st, "POST", &ack, Some(&session), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _) = send(&st, "POST", &ack, Some(&session), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn run_test_payload_round_trips_through_queue() {
    let st = state().await;
    let (device_id, session) = enrolled_session(&st).await;

    let payload = json!({
        "test_id": "tst_01HZ8R9F0J2YK3P5Q7S9V1W3X5",
        "server_host": "10.0.0.1",
        "test_plan": { "name": "probe_only_v1", "version": 1, "duration_sec": 60,
                       "flows": [{ "flow_id": 1, "type": "probe", "rate_pps": 10 }] },
        "flow_bindings": [{ "flow_id": 1, "server_port": 41000 }]
    });
    commands::enqueue(&st.pool, &device_id, Command::RunTest, Some(&payload)).await.unwrap();

    let path = format!("/api/v1/devices/{device_id}/command");
    let (s, body) = send(&st, "GET", &path, Some(&session), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["command"], "RUN_TEST");
    // payload survives round-trip and parses as the contract DTO
    let rt: RunTestPayloadDe = serde_json::from_value(body["payload"].clone()).unwrap();
    assert_eq!(rt.test_plan.flows.len(), 1);
    assert_eq!(rt.flow_bindings.len(), 1);
}

// Minimal local mirror of RunTestPayload (the full DTO lands in a later sub-phase).
#[derive(serde::Deserialize)]
struct RunTestPayloadDe {
    test_plan: TestPlan,
    flow_bindings: Vec<FlowBinding>,
}

#[tokio::test]
async fn status_upserts() {
    let st = state().await;
    let (device_id, session) = enrolled_session(&st).await;

    let status = json!({
        "logger_state": "RUNNING_TEST",
        "current_test_id": "tst_01HZ8R9F0J2YK3P5Q7S9V1W3X5",
        "logging_config_version": 1,
        "sampled_at": { "mono_ns": 123, "utc_ns": 1747987200000000000_i64 },
        "battery": { "level_pct": 79, "charging": false },
        "storage": { "free_bytes": 40000000000_i64 },
        "pending_uploads": 2,
        "radio": { "pci": 305, "rsrp": -103 }
    });
    let path = format!("/api/v1/devices/{device_id}/status");
    let (s, _) = send(&st, "POST", &path, Some(&session), Some(status)).await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    let (state_val, batt): (String, i64) = sqlx::query_as(
        "SELECT logger_state, battery_level_pct FROM device_status WHERE device_id = ?",
    )
    .bind(&device_id)
    .fetch_one(&st.pool)
    .await
    .unwrap();
    assert_eq!(state_val, "RUNNING_TEST");
    assert_eq!(batt, 79);
}
