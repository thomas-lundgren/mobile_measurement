//! test-control binary — serves the internal control API on loopback.

use mp_test_control::{build_router, AppState, Config};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bind = std::env::var("MP_TEST_CONTROL_BIND").unwrap_or_else(|_| "127.0.0.1:9000".to_string());
    let cfg = Config {
        generator_base_url: std::env::var("MP_GENERATOR_URL").unwrap_or_else(|_| "http://127.0.0.1:9100".to_string()),
        log_root: std::env::var("MP_LOG_ROOT").unwrap_or_else(|_| "/var/lib/mp".to_string()).into(),
        port_base: 41000,
        port_count: 100,
    };
    let app = build_router(AppState::new(cfg));
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    println!("test-control internal API on http://{bind}");
    axum::serve(listener, app).await?;
    Ok(())
}
