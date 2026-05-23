//! Phase-1 exit criteria: a full probe test end to end, no Android.
//!
//! Wires real data-generator + test-control servers and drives server-main
//! (via oneshot) as both operator and device:
//!   operator login → device enroll/challenge/verify/register
//!   → operator POST /tests → device polls RUN_TEST → device probes the bound
//!   port (echoes) → device acks → device uploads (summary/PUT/complete)
//!   → COMPLETE.

use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use fieldtest_core::auth as core_auth;
use fieldtest_core::packet::ProbePacket;
use fieldtest_core::timestamp::Timestamp;
use mp_contracts::*;
use mp_server_main::{auth, build_router, db, AppState};
use p256::ecdsa::SigningKey;
use serde_json::json;
use sha2::{Digest, Sha256};
use tower::ServiceExt;

async fn spawn<F>(make: F) -> String
where
    F: FnOnce() -> axum::Router,
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = make();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

async fn call(st: &AppState, method: &str, path: &str, bearer: Option<&str>, body: Body, ctype: Option<&str>) -> (StatusCode, Vec<u8>) {
    let mut b = Request::builder().method(method).uri(path);
    if let Some(t) = bearer { b = b.header("authorization", format!("Bearer {t}")); }
    if let Some(c) = ctype { b = b.header("content-type", c); }
    let resp = build_router(st.clone()).oneshot(b.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap().to_vec();
    (status, bytes)
}

async fn jcall(st: &AppState, method: &str, path: &str, bearer: Option<&str>, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let (s, bytes) = call(st, method, path, bearer, Body::from(body.to_string()), Some("application/json")).await;
    (s, serde_json::from_slice(&bytes).unwrap_or(json!(null)))
}

#[tokio::test]
async fn probe_end_to_end() {
    // --- stand up the data path services ------------------------------------
    let gen_url = spawn(|| mp_data_generator::build_router(mp_data_generator::AppState::default())).await;
    let logs = tempfile::tempdir().unwrap();
    let tc_state = mp_test_control::AppState::new(mp_test_control::Config {
        generator_base_url: gen_url,
        log_root: logs.path().to_path_buf(),
        port_base: 47000,
        port_count: 50,
    });
    let tc_url = spawn(move || mp_test_control::build_router(tc_state)).await;

    let dbdir = tempfile::tempdir().unwrap();
    let uploads = tempfile::tempdir().unwrap();
    let pool = db::connect(&format!("sqlite://{}/mp.db", dbdir.path().display())).await.unwrap();
    let st = AppState::new(pool, "secret", "srv_test_01")
        .with_server_host("127.0.0.1")
        .with_test_control_url(tc_url)
        .with_upload_root(uploads.path().to_path_buf());

    // --- operator + device onboarding ---------------------------------------
    auth::create_operator(&st.pool, "op", "pw", OperatorRole::Operator).await.unwrap();
    let (_, b) = jcall(&st, "POST", "/api/v1/auth/login", None, json!({"username":"op","password":"pw"})).await;
    let jwt = b["jwt"].as_str().unwrap().to_string();

    let sk: SigningKey = { let s: [u8; 32] = std::array::from_fn(|i| (i + 5) as u8); SigningKey::from_bytes(&s.into()).unwrap() };
    mp_server_main::registry::mint_enrollment_token(&st.pool, &mp_server_main::util::hash_token("etok"), 0, i64::MAX).await.unwrap();
    let (_, b) = jcall(&st, "POST", "/api/v1/devices/enroll", Some("etok"), json!({
        "device_pubkey": core_auth::public_key_spki_b64(&sk),
        "pubkey_format": AUTH_PUBKEY_FORMAT, "sig_alg": AUTH_SIGNING_ALG, "sig_format": AUTH_SIG_FORMAT,
        "platform": "android", "hw_info": {}, "protocol_version": PROTOCOL_VERSION
    })).await;
    let device_id = b["device_id"].as_str().unwrap().to_string();

    let (_, b) = jcall(&st, "POST", "/api/v1/auth/challenge", None, json!({"device_id": device_id})).await;
    let (cid, nonce) = (b["challenge_id"].as_str().unwrap().to_string(), b["nonce"].as_str().unwrap().to_string());
    let sig = core_auth::sign_der_b64(&sk, &core_auth::build_message(AUTH_DOMAIN, "srv_test_01", &cid, &nonce));
    let (_, b) = jcall(&st, "POST", "/api/v1/auth/verify", None, json!({"challenge_id": cid, "signature": sig})).await;
    let session = b["session_token"].as_str().unwrap().to_string();

    let (s, _) = jcall(&st, "POST", "/api/v1/devices/register", Some(&session), json!({
        "radio": {"serving_cell":["pci","nci","rsrp","rsrq","sinr"],"neighbor_cells":true,"lte_fallback":true,
                  "cqi":false,"mcs":false,"bler":false,"harq":false,"drx":false,"bwp":false,"mimo_layers":false,"tx_power":false},
        "qos": {"5qi":false,"qfi":false,"slice_info":false},
        "mobility": {"explicit_ho_events":false,"rrc_state":false,"beam_switch":false},
        "timestamp_sources": ["mono","utc","gps"]
    })).await;
    assert_eq!(s, StatusCode::OK);

    // --- operator starts a probe test ---------------------------------------
    let (s, ts) = jcall(&st, "POST", "/api/v1/tests", Some(&jwt), json!({
        "device_id": device_id,
        "plan": { "name": "probe_only_v1", "version": 1, "duration_sec": 60,
                  "flows": [{ "flow_id": 1, "type": "probe", "rate_pps": 10 }] }
    })).await;
    assert_eq!(s, StatusCode::CREATED, "create test: {ts}");
    let test_id = ts["test_id"].as_str().unwrap().to_string();
    assert_eq!(ts["state"], "WAITING_FOR_CLIENT");

    // --- device polls the RUN_TEST command ----------------------------------
    let (s, cmd) = call(&st, "GET", &format!("/api/v1/devices/{device_id}/command"), Some(&session), Body::empty(), None).await;
    assert_eq!(s, StatusCode::OK);
    let cmd: serde_json::Value = serde_json::from_slice(&cmd).unwrap();
    assert_eq!(cmd["command"], "RUN_TEST");
    let payload: RunTestPayload = serde_json::from_value(cmd["payload"].clone()).unwrap();
    assert_eq!(payload.test_id, test_id);
    let port = payload.flow_bindings[0].server_port;
    let server_host = payload.server_host.clone();

    // --- device runs the probe flow against the generator -------------------
    let ue = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    ue.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
    let target = format!("{server_host}:{port}");
    let mut echoes = 0;
    for seq in 1..=15u64 {
        let pkt = ProbePacket { flow_id: 1, sequence_number: seq, send_mono_ns: Timestamp::now().mono_ns };
        ue.send_to(&pkt.to_bytes(), &target).unwrap();
        let mut buf = [0u8; 2048];
        if ue.recv_from(&mut buf).is_ok() { echoes += 1; }
    }
    assert!(echoes >= 12, "probe echoes via the orchestrated generator: {echoes}/15");

    // ack the command
    let cmd_id = cmd["command_id"].as_i64().unwrap();
    let (s, _) = call(&st, "POST", &format!("/api/v1/devices/{device_id}/command/{cmd_id}/ack"), Some(&session), Body::empty(), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // --- device uploads its results -----------------------------------------
    let packets = b"mobile-packets-parquet-bytes".to_vec();
    let sha: String = Sha256::digest(&packets).iter().map(|b| format!("{b:02x}")).collect();
    let (s, init) = jcall(&st, "POST", &format!("/api/v1/tests/{test_id}/summary"), Some(&session), json!({
        "test_result": "COMPLETED", "node": "mobile",
        "files": [{ "path": "mobile/packets.parquet", "size_bytes": packets.len(), "sha256": sha, "encoding": "identity" }]
    })).await;
    assert_eq!(s, StatusCode::OK, "summary: {init}");
    let upload_id = init["upload_id"].as_str().unwrap().to_string();

    let url = format!("/api/v1/tests/{test_id}/upload/{upload_id}/files/mobile/packets.parquet");
    let (s, _) = call(&st, "PUT", &url, Some(&session), Body::from(packets), Some("application/octet-stream")).await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    let (s, _) = call(&st, "POST", &format!("/api/v1/tests/{test_id}/upload/{upload_id}/complete"), Some(&session), Body::empty(), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // --- assertions ----------------------------------------------------------
    let up_state: String = sqlx::query_scalar("SELECT state FROM uploads WHERE test_id = ?").bind(&test_id).fetch_one(&st.pool).await.unwrap();
    assert_eq!(up_state, "COMPLETE");

    // both nodes' files exist under one test tree: mobile (uploaded) + server (logged)
    let session_id: String = sqlx::query_scalar("SELECT session_id FROM tests WHERE test_id = ?").bind(&test_id).fetch_one(&st.pool).await.unwrap();
    assert!(uploads.path().join(&session_id).join(&test_id).join("mobile/packets.parquet").exists(), "mobile file assembled");
    let server_dir = logs.path().join(&session_id).join(&test_id).join("server");
    let server_parquet = std::fs::read_dir(&server_dir).map(|rd| rd.filter_map(|e| e.ok()).any(|e| e.path().extension().map(|x| x == "parquet").unwrap_or(false))).unwrap_or(false);
    assert!(server_parquet, "server-side packets.parquet written at {server_dir:?}");
}
