//! Upload protocol integration tests — the 3-phase resumable flow through the
//! real router. A test row + session must exist (server-main creates these in
//! the full flow); here we insert them directly.

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use fieldtest_core::auth as core_auth;
use mp_contracts::*;
use mp_server_main::{build_router, db, AppState};
use p256::ecdsa::SigningKey;
use serde_json::json;
use sha2::{Digest, Sha256};
use tower::ServiceExt;

async fn state(upload_root: &std::path::Path) -> AppState {
    let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    let url = format!("sqlite://{}/mp.db", dir.path().display());
    AppState::new(
        db::connect(&url).await.unwrap(),
        "test-secret",
        "srv_test_01",
    )
    .with_upload_root(upload_root.to_path_buf())
}

async fn req(
    state: &AppState,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: Body,
    ctype: Option<&str>,
) -> (StatusCode, Vec<u8>) {
    let mut b = Request::builder().method(method).uri(path);
    if let Some(t) = bearer {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    if let Some(c) = ctype {
        b = b.header("content-type", c);
    }
    let resp = build_router(state.clone())
        .oneshot(b.body(body).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec();
    (status, bytes)
}

async fn json_req(
    state: &AppState,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let (s, bytes) = req(
        state,
        method,
        path,
        bearer,
        Body::from(body.to_string()),
        Some("application/json"),
    )
    .await;
    let v = serde_json::from_slice(&bytes).unwrap_or(json!(null));
    (s, v)
}

/// Enroll + authenticate a device, returning its session token + device_id.
async fn device_session(st: &AppState) -> (String, String) {
    let sk: SigningKey = {
        let secret: [u8; 32] = std::array::from_fn(|i| (i + 3) as u8);
        SigningKey::from_bytes(&secret.into()).unwrap()
    };
    mp_server_main::registry::mint_enrollment_token(
        &st.pool,
        &mp_server_main::util::hash_token("tok"),
        0,
        i64::MAX,
    )
    .await
    .unwrap();
    let (_, b) = json_req(st, "POST", "/api/v1/devices/enroll", Some("tok"), json!({
        "device_pubkey": core_auth::public_key_spki_b64(&sk),
        "pubkey_format": AUTH_PUBKEY_FORMAT, "sig_alg": AUTH_SIGNING_ALG, "sig_format": AUTH_SIG_FORMAT,
        "platform": "android", "hw_info": {}, "protocol_version": PROTOCOL_VERSION
    })).await;
    let device_id = b["device_id"].as_str().unwrap().to_string();
    let (_, b) = json_req(
        st,
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
    let (_, b) = json_req(
        st,
        "POST",
        "/api/v1/auth/verify",
        None,
        json!({"challenge_id": cid, "signature": sig}),
    )
    .await;
    (device_id, b["session_token"].as_str().unwrap().to_string())
}

/// Create a session + test row owned by the device (server-main does this in 1i).
async fn make_test(st: &AppState, device_id: &str, test_id: &str) {
    sqlx::query("INSERT INTO sessions(session_id, device_id, started_at) VALUES('ses_up', ?, 0)")
        .bind(device_id)
        .execute(&st.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tests(test_id, session_id, device_id, plan_name, plan_version, plan_json, server_host, logging_config_json, logging_config_version, state, created_at) \
         VALUES (?, 'ses_up', ?, 'p', 1, '{}', 'h', '{}', 1, 'WAITING_FOR_CLIENT', 0)",
    )
    .bind(test_id).bind(device_id).execute(&st.pool).await.unwrap();
}

fn manifest_file(path: &str, bytes: &[u8]) -> serde_json::Value {
    let sha: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    json!({ "path": path, "size_bytes": bytes.len(), "sha256": sha, "encoding": "identity" })
}

#[tokio::test]
async fn full_upload_then_complete() {
    let root = tempfile::tempdir().unwrap();
    let st = state(root.path()).await;
    let (device_id, session) = device_session(&st).await;
    let test_id = "tst_01HZ8R9F0J2YK3P5Q7S9V1W3X5";
    make_test(&st, &device_id, test_id).await;

    let header = br#"{"schema_version":1}"#.to_vec();
    let packets = b"PARQUETBYTES-mobile-packets".to_vec();
    let manifest = json!({
        "test_result": "COMPLETED", "node": "mobile",
        "files": [ manifest_file("header.json", &header), manifest_file("mobile/packets.parquet", &packets) ]
    });

    // phase 1: summary → upload_id + both files needed
    let (s, body) = json_req(
        &st,
        "POST",
        &format!("/api/v1/tests/{test_id}/summary"),
        Some(&session),
        manifest,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "summary: {body}");
    let upload_id = body["upload_id"].as_str().unwrap().to_string();
    assert_eq!(body["needed_paths"].as_array().unwrap().len(), 2);

    // phase 2: PUT both files
    for (path, bytes) in [
        ("header.json", &header),
        ("mobile/packets.parquet", &packets),
    ] {
        let url = format!("/api/v1/tests/{test_id}/upload/{upload_id}/files/{path}");
        let (s, _) = req(
            &st,
            "PUT",
            &url,
            Some(&session),
            Body::from(bytes.clone()),
            Some("application/octet-stream"),
        )
        .await;
        assert_eq!(s, StatusCode::NO_CONTENT, "put {path}");
    }

    // phase 3: complete
    let (s, _) = req(
        &st,
        "POST",
        &format!("/api/v1/tests/{test_id}/upload/{upload_id}/complete"),
        Some(&session),
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // tree assembled under upload_root/session/test/...
    assert!(root
        .path()
        .join("ses_up")
        .join(test_id)
        .join("header.json")
        .exists());
    assert!(root
        .path()
        .join("ses_up")
        .join(test_id)
        .join("mobile/packets.parquet")
        .exists());
    // upload marked COMPLETE
    let st_str: String = sqlx::query_scalar("SELECT state FROM uploads WHERE test_id = ?")
        .bind(test_id)
        .fetch_one(&st.pool)
        .await
        .unwrap();
    assert_eq!(st_str, "COMPLETE");

    // completing the upload finalizes the test: WAITING_FOR_CLIENT -> DONE,
    // result COMPLETED, ended_at stamped.
    let (test_state, result, ended): (String, Option<String>, Option<i64>) =
        sqlx::query_as("SELECT state, result, ended_at FROM tests WHERE test_id = ?")
            .bind(test_id)
            .fetch_one(&st.pool)
            .await
            .unwrap();
    assert_eq!(test_state, "DONE");
    assert_eq!(result.as_deref(), Some("COMPLETED"));
    assert!(ended.is_some(), "ended_at stamped on finalize");
}

#[tokio::test]
async fn resume_returns_only_missing() {
    let root = tempfile::tempdir().unwrap();
    let st = state(root.path()).await;
    let (device_id, session) = device_session(&st).await;
    let test_id = "tst_01HZ8R9F0J2YK3P5Q7S9V1W3X6";
    make_test(&st, &device_id, test_id).await;

    let a = b"file-a".to_vec();
    let b = b"file-b-bytes".to_vec();
    let manifest = json!({ "test_result": "COMPLETED", "node": "mobile",
        "files": [ manifest_file("header.json", &a), manifest_file("mobile/clock.parquet", &b) ] });

    let (_, body) = json_req(
        &st,
        "POST",
        &format!("/api/v1/tests/{test_id}/summary"),
        Some(&session),
        manifest.clone(),
    )
    .await;
    let upload_id = body["upload_id"].as_str().unwrap().to_string();

    // upload only header.json
    let url = format!("/api/v1/tests/{test_id}/upload/{upload_id}/files/header.json");
    req(
        &st,
        "PUT",
        &url,
        Some(&session),
        Body::from(a.clone()),
        None,
    )
    .await;

    // re-summary → only the clock file remains; same upload_id
    let (_, body2) = json_req(
        &st,
        "POST",
        &format!("/api/v1/tests/{test_id}/summary"),
        Some(&session),
        manifest,
    )
    .await;
    assert_eq!(body2["upload_id"], upload_id, "idempotent upload_id");
    let needed = body2["needed_paths"].as_array().unwrap();
    assert_eq!(needed.len(), 1);
    assert_eq!(needed[0], "mobile/clock.parquet");

    // complete now fails (missing) → 409
    let (s, _) = req(
        &st,
        "POST",
        &format!("/api/v1/tests/{test_id}/upload/{upload_id}/complete"),
        Some(&session),
        Body::empty(),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
}

#[tokio::test]
async fn hash_mismatch_rejected() {
    let root = tempfile::tempdir().unwrap();
    let st = state(root.path()).await;
    let (device_id, session) = device_session(&st).await;
    let test_id = "tst_01HZ8R9F0J2YK3P5Q7S9V1W3X7";
    make_test(&st, &device_id, test_id).await;

    let good = b"the-real-bytes".to_vec();
    let manifest = json!({ "test_result": "COMPLETED", "node": "mobile",
        "files": [ manifest_file("header.json", &good) ] });
    let (_, body) = json_req(
        &st,
        "POST",
        &format!("/api/v1/tests/{test_id}/summary"),
        Some(&session),
        manifest,
    )
    .await;
    let upload_id = body["upload_id"].as_str().unwrap().to_string();

    // PUT different bytes → 422
    let url = format!("/api/v1/tests/{test_id}/upload/{upload_id}/files/header.json");
    let (s, _) = req(
        &st,
        "PUT",
        &url,
        Some(&session),
        Body::from(b"tampered".to_vec()),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn foreign_test_and_unauth_rejected() {
    let root = tempfile::tempdir().unwrap();
    let st = state(root.path()).await;
    let (_dev, session) = device_session(&st).await;
    let manifest = json!({ "test_result": "COMPLETED", "node": "mobile", "files": [] });

    // unauthenticated
    let (s, _) = json_req(
        &st,
        "POST",
        "/api/v1/tests/tst_x/summary",
        None,
        manifest.clone(),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // a test that doesn't exist / isn't owned → 404
    let (s, _) = json_req(
        &st,
        "POST",
        "/api/v1/tests/tst_not_mine/summary",
        Some(&session),
        manifest,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// POST the summary as if from `peer` (the server runs with connect info).
/// Phase 1 from `peer` with a packets-only manifest (nothing a held upload takes).
async fn summary_from(st: &AppState, test_id: &str, session: &str, peer: &str) -> StatusCode {
    let manifest = json!({
        "test_result": "COMPLETED", "node": "mobile",
        "files": [ manifest_file("mobile/packets_0000.parquet", b"PAR1") ]
    });
    summary_with(st, test_id, session, peer, manifest).await.0
}

async fn summary_with(
    st: &AppState,
    test_id: &str,
    session: &str,
    peer: &str,
    manifest: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let addr: std::net::SocketAddr = peer.parse().unwrap();
    let r = Request::builder()
        .method("POST")
        .uri(format!("/api/v1/tests/{test_id}/summary"))
        .header("authorization", format!("Bearer {session}"))
        .header("content-type", "application/json")
        .extension(axum::extract::ConnectInfo(addr))
        .body(Body::from(manifest.to_string()))
        .unwrap();
    let resp = build_router(st.clone()).oneshot(r).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

/// Away from home the server takes only the base files (radio, GPS, …):
/// the test's map and radio show at once, the packet logs wait for home.
#[tokio::test]
async fn held_upload_takes_the_base_files_and_keeps_the_rest_pending() {
    let root = tempfile::tempdir().unwrap();
    let st = state(root.path())
        .await
        .with_server_host("2.248.90.179")
        .with_upload_hold(mp_server_main::UploadHold { min_free_bytes: 0 });
    let (device_id, session) = device_session(&st).await;
    let test_id = "tst_01HZ8R9F0J2YK3P5Q7S9V1W3X5";
    make_test(&st, &device_id, test_id).await;
    let files: [(&str, &[u8]); 4] = [
        ("header.json", b"{}"),
        ("gps.parquet", b"GPS"),
        ("radio.parquet", b"RADIO"),
        ("mobile/packets_0000.parquet", b"PACKETS"),
    ];
    let manifest = json!({
        "test_result": "COMPLETED", "node": "mobile",
        "files": files.iter().map(|(p, b)| manifest_file(p, b)).collect::<Vec<_>>()
    });
    let away = "90.129.122.158:40000";

    let (s, body) = summary_with(&st, test_id, &session, away, manifest.clone()).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(
        body["needed_paths"],
        json!(["header.json", "gps.parquet", "radio.parquet"])
    );
    let upload_id = body["upload_id"].as_str().unwrap().to_string();
    for (path, bytes) in &files[..3] {
        let url = format!("/api/v1/tests/{test_id}/upload/{upload_id}/files/{path}");
        let (s, _) = req(
            &st,
            "PUT",
            &url,
            Some(&session),
            Body::from(bytes.to_vec()),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::NO_CONTENT, "put {path}");
    }
    // the packets are still missing: not complete, so the device keeps the test pending
    let complete = format!("/api/v1/tests/{test_id}/upload/{upload_id}/complete");
    let (s, _) = req(&st, "POST", &complete, Some(&session), Body::empty(), None).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(root
        .path()
        .join("ses_up")
        .join(test_id)
        .join("radio.parquet")
        .exists());

    // still away, base stored: held (retry after a network change)
    let (s, _) = summary_with(&st, test_id, &session, away, manifest.clone()).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    // home: the rest
    let (s, body) = summary_with(&st, test_id, &session, "192.168.1.57:40000", manifest).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["needed_paths"], json!(["mobile/packets_0000.parquet"]));
}

#[tokio::test]
async fn upload_held_off_the_local_network_unless_storage_is_low() {
    let root = tempfile::tempdir().unwrap();
    let st = state(root.path())
        .await
        .with_server_host("2.248.90.179")
        .with_upload_hold(mp_server_main::UploadHold {
            min_free_bytes: 2_000_000_000,
        });
    let (device_id, session) = device_session(&st).await;
    let test_id = "tst_01HZ8R9F0J2YK3P5Q7S9V1W3X5";
    make_test(&st, &device_id, test_id).await;

    // mobile network (public or carrier-grade NAT address): held, retryable
    for peer in ["90.129.122.158:40000", "100.70.1.2:40000"] {
        assert_eq!(
            summary_from(&st, test_id, &session, peer).await,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
    // home Wi-Fi: a LAN address, or the router's hairpin NAT showing our own public address
    for peer in ["192.168.1.57:40000", "2.248.90.179:40000"] {
        assert_eq!(
            summary_from(&st, test_id, &session, peer).await,
            StatusCode::OK
        );
    }
    // storage running low: accepted over the mobile network too
    let status = |free| mp_server_main::registry::StatusUpdate {
        device_id: device_id.clone(),
        logger_state: "IDLE".into(),
        current_test_id: None,
        logging_config_version: 1,
        battery_level_pct: None,
        battery_charging: None,
        storage_free_bytes: Some(free),
        pending_uploads: None,
        radio_json: None,
        sampled_mono_ns: 1,
        sampled_utc_ns: 2,
        received_at: 3,
    };
    mp_server_main::registry::upsert_device_status(&st.pool, &status(50_000_000_000))
        .await
        .unwrap();
    assert_eq!(
        summary_from(&st, test_id, &session, "90.129.122.158:40000").await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    mp_server_main::registry::upsert_device_status(&st.pool, &status(1_000_000_000))
        .await
        .unwrap();
    assert_eq!(
        summary_from(&st, test_id, &session, "90.129.122.158:40000").await,
        StatusCode::OK
    );
}
