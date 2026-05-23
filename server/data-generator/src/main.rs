//! data-generator binary — serves the internal control API on loopback.

use mp_data_generator::{build_router, AppState};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bind = std::env::var("MP_GENERATOR_BIND").unwrap_or_else(|_| "127.0.0.1:9100".to_string());
    let app = build_router(AppState::default());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    println!("data-generator internal API on http://{bind}");
    axum::serve(listener, app).await?;
    Ok(())
}
