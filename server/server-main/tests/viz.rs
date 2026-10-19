//! Coverage map: the page is public (no data in it), the track needs an
//! operator JWT and is served from `<upload_root>/viz/track.json`.

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use mp_contracts::OperatorRole;
use mp_server_main::{auth, build_router, db, AppState};
use serde_json::{json, Value};
use tower::ServiceExt;

async fn state() -> (AppState, std::path::PathBuf) {
    let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
    let url = format!("sqlite://{}/mp.db", dir.path().display());
    let root = dir.path().join("uploads");
    let st = AppState::new(
        db::connect(&url).await.unwrap(),
        "test-secret",
        "srv_test_01",
    )
    .with_upload_root(&root);
    (st, root)
}

async fn get(st: &AppState, path: &str, bearer: Option<&str>) -> (StatusCode, Vec<u8>) {
    let mut req = Request::builder().uri(path);
    if let Some(b) = bearer {
        req = req.header("authorization", format!("Bearer {b}"));
    }
    let resp = build_router(st.clone())
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    (
        status,
        to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
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

#[tokio::test]
async fn page_is_served_without_auth() {
    let (st, _) = state().await;
    let (s, body) = get(&st, "/viz", None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(String::from_utf8(body)
        .unwrap()
        .contains("/api/v1/viz/track"));
}

#[tokio::test]
async fn track_requires_operator_and_serves_file() {
    let (st, root) = state().await;
    let (s, _) = get(&st, "/api/v1/viz/track", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    let jwt = operator_jwt(&st).await;
    let (s, _) = get(&st, "/api/v1/viz/track", Some(&jwt)).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "no track file yet");

    std::fs::create_dir_all(root.join("viz")).unwrap();
    std::fs::write(root.join("viz/track.json"), r#"{"points":[{"lat":55.7}]}"#).unwrap();
    let (s, body) = get(&st, "/api/v1/viz/track", Some(&jwt)).await;
    assert_eq!(s, StatusCode::OK);
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["points"][0]["lat"], 55.7);
}

#[tokio::test]
async fn series_days_and_day_file() {
    let (st, root) = state().await;
    let jwt = operator_jwt(&st).await;
    let (s, _) = get(&st, "/viz/charts", None).await;
    assert_eq!(s, StatusCode::OK);
    let (s, body) = get(&st, "/favicon.png", None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(body.starts_with(b"\x89PNG"));
    let (s, _) = get(&st, "/api/v1/viz/series/2026-10-19", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    std::fs::create_dir_all(root.join("viz/series")).unwrap();
    std::fs::write(
        root.join("viz/series/days.json"),
        r#"{"days":["2026-10-19"]}"#,
    )
    .unwrap();
    std::fs::write(root.join("viz/series/2026-10-19.json"), r#"{"t":[1.0]}"#).unwrap();
    let (s, body) = get(&st, "/api/v1/viz/series", Some(&jwt)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["days"][0],
        "2026-10-19"
    );
    let (s, body) = get(&st, "/api/v1/viz/series/2026-10-19", Some(&jwt)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap()["t"][0], 1.0);

    std::fs::create_dir_all(root.join("viz/findings")).unwrap();
    std::fs::write(
        root.join("viz/findings/2026-10-19.json"),
        r#"{"day":"2026-10-19"}"#,
    )
    .unwrap();
    let (s, body) = get(&st, "/api/v1/viz/findings/2026-10-19", Some(&jwt)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["day"],
        "2026-10-19"
    );
    let (s, _) = get(&st, "/api/v1/viz/findings/2026-10-19", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = get(&st, "/viz/findings", None).await;
    assert_eq!(s, StatusCode::OK);

    for bad in ["..%2Fdays", "2026-10-1x", "20261019000"] {
        let (s, _) = get(&st, &format!("/api/v1/viz/series/{bad}"), Some(&jwt)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
    }
}

#[tokio::test]
async fn timing_detail_per_test() {
    let (st, root) = state().await;
    let jwt = operator_jwt(&st).await;
    std::fs::create_dir_all(root.join("viz/timing")).unwrap();
    std::fs::write(root.join("viz/timing/tst_01ABC.json"), r#"{"streams":[]}"#).unwrap();
    let (s, _) = get(&st, "/api/v1/viz/timing/tst_01ABC", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, body) = get(&st, "/api/v1/viz/timing/tst_01ABC", Some(&jwt)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(serde_json::from_slice::<Value>(&body).unwrap()["streams"].is_array());
    let (s, _) = get(&st, "/api/v1/viz/timing/tst_..%2Fsecret", Some(&jwt)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = get(&st, "/api/v1/viz/timing/tst_MISSING", Some(&jwt)).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}
