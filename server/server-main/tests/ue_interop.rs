//! Phase-2 conformance proof (exit criterion 2): the shared `core` UE engine —
//! the same engine that will run on the Pixel — drives a full multi-flow test
//! against the REAL server-main + test-control + data-generator, with no
//! Android. Proves the mobile traffic engine interoperates with the server for
//! every flow type and direction, and writes valid local Parquet.

use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use fieldtest_core::auth as core_auth;
use fieldtest_core::ue::{UeEngine, UeFlow, UeTestConfig};
use mp_contracts::*;
use mp_server_main::{build_router, db, AppState};
use p256::ecdsa::SigningKey;
use serde_json::json;
use tower::ServiceExt;

async fn spawn(make: impl FnOnce() -> axum::Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = make();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

async fn jcall(
    st: &AppState,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json");
    if let Some(t) = bearer {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    let resp = build_router(st.clone())
        .oneshot(b.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(json!(null)),
    )
}

async fn get_bytes(st: &AppState, path: &str, bearer: &str) -> (StatusCode, Vec<u8>) {
    let req = Request::builder()
        .method("GET")
        .uri(path)
        .header("authorization", format!("Bearer {bearer}"))
        .body(Body::empty())
        .unwrap();
    let resp = build_router(st.clone()).oneshot(req).await.unwrap();
    (
        resp.status(),
        to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
}

fn dir_str(d: Direction) -> String {
    match d {
        Direction::FromServer => "FromServer".into(),
        Direction::ToServer => "ToServer".into(),
    }
}

/// Merge the contract RUN_TEST payload (port-less flows + bindings) into the
/// engine's lean config — exactly what the Java shell will do at the JNI seam.
fn to_ue_config(p: &RunTestPayload, device_id: &str, log_dir: &str) -> UeTestConfig {
    let port_of = |fid: u32| {
        p.flow_bindings
            .iter()
            .find(|b| b.flow_id == fid)
            .unwrap()
            .server_port
    };
    let flows = p
        .test_plan
        .flows
        .iter()
        .map(|f| match f {
            Flow::Probe {
                flow_id,
                rate_pps,
                packet_size_bytes,
            } => UeFlow {
                flow_id: *flow_id,
                flow_type: "probe".into(),
                direction: None,
                server_port: port_of(*flow_id),
                rate_pps: Some(*rate_pps),
                packet_size_bytes: Some(*packet_size_bytes),
                target_bytes: None,
            },
            Flow::RealtimeUdp {
                flow_id,
                direction,
                rate_pps,
                packet_size_bytes,
            } => UeFlow {
                flow_id: *flow_id,
                flow_type: "realtime_udp".into(),
                direction: Some(dir_str(*direction)),
                server_port: port_of(*flow_id),
                rate_pps: Some(*rate_pps),
                packet_size_bytes: Some(*packet_size_bytes),
                target_bytes: None,
            },
            Flow::BulkTcp {
                flow_id,
                direction,
                target_bytes,
            } => UeFlow {
                flow_id: *flow_id,
                flow_type: "bulk_tcp".into(),
                direction: Some(dir_str(*direction)),
                server_port: port_of(*flow_id),
                rate_pps: None,
                packet_size_bytes: None,
                target_bytes: target_bytes.map(|t| t as u64),
            },
        })
        .collect();
    UeTestConfig {
        test_id: p.test_id.clone(),
        device_id: device_id.into(),
        // Device-local session id (the mobile groups its own tests). test_id is
        // the cross-node key; see the note in the test summary.
        session_id: "ses_mobile_local".into(),
        server_host: p.server_host.clone(),
        log_dir: log_dir.into(),
        segment_rows: 1000,
        flows,
    }
}

fn parquet_count(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| {
                    e.path()
                        .extension()
                        .map(|x| x == "parquet")
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0)
}

#[tokio::test]
async fn ue_engine_interops_with_live_server_all_flows() {
    // --- live data path services ---
    let gen_url =
        spawn(|| mp_data_generator::build_router(mp_data_generator::AppState::default())).await;
    let tc_logs = tempfile::tempdir().unwrap();
    let tc_state = mp_test_control::AppState::new(mp_test_control::Config {
        generator_base_url: gen_url,
        log_root: tc_logs.path().to_path_buf(),
        port_base: 47000,
        port_count: 50,
    });
    let tc_url = spawn(move || mp_test_control::build_router(tc_state)).await;

    let dbdir = tempfile::tempdir().unwrap();
    let pool = db::connect(&format!("sqlite://{}/mp.db", dbdir.path().display()))
        .await
        .unwrap();
    let st = AppState::new(pool, "secret", "srv_test_01")
        .with_server_host("127.0.0.1")
        .with_test_control_url(tc_url);

    // --- operator + device onboarding ---
    mp_server_main::auth::create_operator(&st.pool, "op", "pw", OperatorRole::Operator)
        .await
        .unwrap();
    let (_, b) = jcall(
        &st,
        "POST",
        "/api/v1/auth/login",
        None,
        json!({"username":"op","password":"pw"}),
    )
    .await;
    let jwt = b["jwt"].as_str().unwrap().to_string();

    let sk: SigningKey = {
        let s: [u8; 32] = std::array::from_fn(|i| (i + 13) as u8);
        SigningKey::from_bytes(&s.into()).unwrap()
    };
    mp_server_main::registry::mint_enrollment_token(
        &st.pool,
        &mp_server_main::util::hash_token("etok"),
        0,
        i64::MAX,
    )
    .await
    .unwrap();
    let (_, b) = jcall(&st, "POST", "/api/v1/devices/enroll", Some("etok"), json!({
        "device_pubkey": core_auth::public_key_spki_b64(&sk),
        "pubkey_format": AUTH_PUBKEY_FORMAT, "sig_alg": AUTH_SIGNING_ALG, "sig_format": AUTH_SIG_FORMAT,
        "platform": "android", "hw_info": {}, "protocol_version": PROTOCOL_VERSION
    })).await;
    let device_id = b["device_id"].as_str().unwrap().to_string();
    let (_, b) = jcall(
        &st,
        "POST",
        "/api/v1/auth/challenge",
        None,
        json!({"device_id": device_id}),
    )
    .await;
    let (cid, nonce) = (
        b["challenge_id"].as_str().unwrap().to_string(),
        b["nonce"].as_str().unwrap().to_string(),
    );
    let sig = core_auth::sign_der_b64(
        &sk,
        &core_auth::build_message(AUTH_DOMAIN, "srv_test_01", &cid, &nonce),
    );
    let (_, b) = jcall(
        &st,
        "POST",
        "/api/v1/auth/verify",
        None,
        json!({"challenge_id": cid, "signature": sig}),
    )
    .await;
    let session = b["session_token"].as_str().unwrap().to_string();
    jcall(&st, "POST", "/api/v1/devices/register", Some(&session), json!({
        "radio": {"serving_cell":["pci","nci","rsrp","rsrq","sinr"],"neighbor_cells":true,"lte_fallback":true,
                  "cqi":false,"mcs":false,"bler":false,"harq":false,"drx":false,"bwp":false,"mimo_layers":false,"tx_power":false},
        "qos": {"5qi":false,"qfi":false,"slice_info":false},
        "mobility": {"explicit_ho_events":false,"rrc_state":false,"beam_switch":false},
        "timestamp_sources": ["mono","utc","gps"]
    })).await;

    // --- operator starts a test exercising every flow type + direction ---
    let (s, ts) = jcall(&st, "POST", "/api/v1/tests", Some(&jwt), json!({
        "device_id": device_id,
        "plan": { "name": "interop_all_v1", "version": 1, "duration_sec": 60, "flows": [
            { "flow_id": 1, "type": "probe", "rate_pps": 50 },
            { "flow_id": 2, "type": "realtime_udp", "direction": "FromServer", "rate_pps": 100, "packet_size_bytes": 120 },
            { "flow_id": 3, "type": "realtime_udp", "direction": "ToServer", "rate_pps": 100, "packet_size_bytes": 120 },
            { "flow_id": 4, "type": "bulk_tcp", "direction": "ToServer" },
            { "flow_id": 5, "type": "bulk_tcp", "direction": "FromServer" }
        ]}
    })).await;
    assert_eq!(s, StatusCode::CREATED, "create test: {ts}");
    let test_id = ts["test_id"].as_str().unwrap().to_string();

    // --- device polls RUN_TEST and drives the CORE engine ---
    let (s, cmd) = get_bytes(
        &st,
        &format!("/api/v1/devices/{device_id}/command"),
        &session,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let cmd: serde_json::Value = serde_json::from_slice(&cmd).unwrap();
    assert_eq!(cmd["command"], "RUN_TEST");
    let payload: RunTestPayload = serde_json::from_value(cmd["payload"].clone()).unwrap();

    let mobile_logs = tempfile::tempdir().unwrap();
    let engine = UeEngine::start(to_ue_config(
        &payload,
        &device_id,
        mobile_logs.path().to_str().unwrap(),
    ))
    .unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    let summ = engine.stop();

    let by_id = |fid: u32| summ.iter().find(|f| f.flow_id == fid).unwrap();

    // probe: originated + echoes came back
    assert!(
        by_id(1).packets_sent >= 10,
        "probe sent {}",
        by_id(1).packets_sent
    );
    assert!(
        by_id(1).packets_received >= 5,
        "probe echoes {}",
        by_id(1).packets_received
    );
    // realtime FromServer: UE received the downlink stream
    assert!(
        by_id(2).packets_received >= 5,
        "rt downlink {}",
        by_id(2).packets_received
    );
    // realtime ToServer: UE sent the uplink stream
    assert!(
        by_id(3).packets_sent >= 10,
        "rt uplink {}",
        by_id(3).packets_sent
    );
    // bulk ToServer: UE uploaded bytes
    assert!(
        by_id(4).bytes_sent >= 256 * 1024,
        "bulk up {}",
        by_id(4).bytes_sent
    );
    // bulk FromServer: UE downloaded bytes
    assert!(
        by_id(5).bytes_received >= 256 * 1024,
        "bulk down {}",
        by_id(5).bytes_received
    );

    // mobile core wrote its packets.parquet (segmented)
    assert!(
        parquet_count(mobile_logs.path()) >= 1,
        "mobile packets.parquet written"
    );

    // server side logged too, under its own session/test/server tree
    let session_id: String = sqlx::query_scalar("SELECT session_id FROM tests WHERE test_id = ?")
        .bind(&test_id)
        .fetch_one(&st.pool)
        .await
        .unwrap();
    let server_dir = tc_logs
        .path()
        .join(&session_id)
        .join(&test_id)
        .join("server");
    assert!(
        parquet_count(&server_dir) >= 1,
        "server packets.parquet at {server_dir:?}"
    );
}
