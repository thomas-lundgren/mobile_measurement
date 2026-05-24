//! server-main binary — control plane (REST API, registry, command queue, uploads).
//! Phase 1: DB (1c) + device/operator auth (1d). Plain HTTP on loopback (TLS is
//! a Phase-5 hardening concern, per the Phase 1 design).

use mp_server_main::{build_router, db, AppState};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database_url =
        std::env::var("MP_DATABASE_URL").unwrap_or_else(|_| "sqlite://mp.db".to_string());
    let jwt_secret =
        std::env::var("MP_JWT_SECRET").unwrap_or_else(|_| "dev-secret-change-me".to_string());
    let server_id = std::env::var("MP_SERVER_ID").unwrap_or_else(|_| "srv_main_01".to_string());
    let bind = std::env::var("MP_BIND").unwrap_or_else(|_| "127.0.0.1:8080".to_string());

    let pool = db::connect(&database_url).await?;
    let mut state = AppState::new(pool, jwt_secret, server_id);
    // Host the UE is told to connect to for data flows (RUN_TEST server_host).
    if let Ok(host) = std::env::var("MP_SERVER_HOST") {
        state = state.with_server_host(host);
    }
    // Internal test-control endpoint; without it POST /tests is rejected.
    if let Ok(url) = std::env::var("MP_TEST_CONTROL_URL") {
        state = state.with_test_control_url(url);
    }
    if let Ok(root) = std::env::var("MP_UPLOAD_ROOT") {
        state = state.with_upload_root(root);
    }
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    println!("server-main listening on http://{bind}");
    axum::serve(listener, app).await?;
    Ok(())
}
