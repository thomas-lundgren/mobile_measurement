//! Auth integration tests — drive the real router in-process via tower oneshot.
//! Covers the full device flow (enroll → challenge → verify → session use) plus
//! failure paths, and operator login.

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use fieldtest_core::auth as core_auth;
use mp_contracts::*;
use mp_server_main::{auth, build_router, db, AppState};
use p256::ecdsa::SigningKey;
use serde_json::json;
use tower::ServiceExt; // oneshot

async fn state() -> AppState {
    let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    let url = format!("sqlite://{}/mp.db", dir.path().display());
    let pool = db::connect(&url).await.unwrap();
    AppState::new(pool, "test-secret", "srv_test_01")
}

async fn post(
    state: &AppState,
    path: &str,
    bearer: Option<&str>,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    if let Some(b) = bearer {
        req = req.header("authorization", format!("Bearer {b}"));
    }
    let req = req.body(Body::from(body.to_string())).unwrap();
    let resp = build_router(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let val = if bytes.is_empty() {
        json!(null)
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, val)
}

fn signing_key() -> SigningKey {
    let secret: [u8; 32] = std::array::from_fn(|i| (i + 7) as u8);
    SigningKey::from_bytes(&secret.into()).unwrap()
}

fn enroll_body(sk: &SigningKey) -> serde_json::Value {
    json!({
        "device_pubkey": core_auth::public_key_spki_b64(sk),
        "pubkey_format": AUTH_PUBKEY_FORMAT,
        "sig_alg": AUTH_SIGNING_ALG,
        "sig_format": AUTH_SIG_FORMAT,
        "platform": "android",
        "hw_info": { "model": "Pixel 9 Pro", "manufacturer": "Google", "os_version": "15" },
        "protocol_version": PROTOCOL_VERSION
    })
}

/// Mint an enrollment token directly (operator tooling does this out of band).
async fn mint(state: &AppState, raw: &str) {
    mp_server_main::registry::mint_enrollment_token(
        &state.pool,
        &mp_server_main::util::hash_token(raw),
        0,
        i64::MAX,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn full_device_flow_then_session_validates() {
    let st = state().await;
    let sk = signing_key();
    mint(&st, "enroll-tok-1").await;

    // enroll
    let (s, body) = post(
        &st,
        "/api/v1/devices/enroll",
        Some("enroll-tok-1"),
        enroll_body(&sk),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "enroll: {body}");
    let device_id = body["device_id"].as_str().unwrap().to_string();
    assert!(device_id.starts_with("dev_"));

    // challenge
    let (s, body) = post(
        &st,
        "/api/v1/auth/challenge",
        None,
        json!({ "device_id": device_id }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "challenge: {body}");
    let challenge_id = body["challenge_id"].as_str().unwrap().to_string();
    let nonce = body["nonce"].as_str().unwrap().to_string();
    assert_eq!(body["auth_domain"], AUTH_DOMAIN);

    // sign the canonical blob and verify
    let msg = core_auth::build_message(AUTH_DOMAIN, "srv_test_01", &challenge_id, &nonce);
    let sig = core_auth::sign_der_b64(&sk, &msg);
    let (s, body) = post(
        &st,
        "/api/v1/auth/verify",
        None,
        json!({ "challenge_id": challenge_id, "signature": sig }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "verify: {body}");
    let session = body["session_token"].as_str().unwrap().to_string();

    // the issued session token validates to the device_id
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        "authorization",
        format!("Bearer {session}").parse().unwrap(),
    );
    assert_eq!(auth::authed_device(&st, &headers).await.unwrap(), device_id);
}

#[tokio::test]
async fn enroll_rejects_bad_token() {
    let st = state().await;
    let sk = signing_key();
    let (s, _) = post(
        &st,
        "/api/v1/devices/enroll",
        Some("never-minted"),
        enroll_body(&sk),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // no device was created (rolled back)
    assert_eq!(
        mp_server_main::registry::list_devices(&st.pool)
            .await
            .unwrap()
            .len(),
        0
    );
}

#[tokio::test]
async fn enroll_token_is_single_use() {
    let st = state().await;
    mint(&st, "tok").await;
    let (s1, _) = post(
        &st,
        "/api/v1/devices/enroll",
        Some("tok"),
        enroll_body(&signing_key()),
    )
    .await;
    assert_eq!(s1, StatusCode::OK);
    let (s2, _) = post(
        &st,
        "/api/v1/devices/enroll",
        Some("tok"),
        enroll_body(&signing_key()),
    )
    .await;
    assert_eq!(s2, StatusCode::UNAUTHORIZED, "token already consumed");
}

#[tokio::test]
async fn verify_rejects_bad_signature() {
    let st = state().await;
    let sk = signing_key();
    mint(&st, "t").await;
    let (_, body) = post(&st, "/api/v1/devices/enroll", Some("t"), enroll_body(&sk)).await;
    let device_id = body["device_id"].as_str().unwrap().to_string();
    let (_, body) = post(
        &st,
        "/api/v1/auth/challenge",
        None,
        json!({ "device_id": device_id }),
    )
    .await;
    let challenge_id = body["challenge_id"].as_str().unwrap().to_string();

    // sign a DIFFERENT message (wrong nonce)
    let wrong =
        core_auth::build_message(AUTH_DOMAIN, "srv_test_01", &challenge_id, "not-the-nonce");
    let sig = core_auth::sign_der_b64(&sk, &wrong);
    let (s, _) = post(
        &st,
        "/api/v1/auth/verify",
        None,
        json!({ "challenge_id": challenge_id, "signature": sig }),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn challenge_unknown_device_is_404() {
    let st = state().await;
    let (s, _) = post(
        &st,
        "/api/v1/auth/challenge",
        None,
        json!({ "device_id": "dev_does_not_exist" }),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn operator_login() {
    let st = state().await;
    auth::create_operator(&st.pool, "alice", "s3cret", OperatorRole::Operator)
        .await
        .unwrap();

    let (s, body) = post(
        &st,
        "/api/v1/auth/login",
        None,
        json!({ "username": "alice", "password": "s3cret" }),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "login: {body}");
    assert_eq!(body["role"], "operator");
    let jwt = body["jwt"].as_str().unwrap().to_string();

    // JWT validates
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("authorization", format!("Bearer {jwt}").parse().unwrap());
    let claims = auth::authed_operator(&st, &headers).unwrap();
    assert_eq!(claims.sub, "alice");
    assert_eq!(claims.role, "operator");

    // wrong password rejected
    let (s, _) = post(
        &st,
        "/api/v1/auth/login",
        None,
        json!({ "username": "alice", "password": "wrong" }),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}
