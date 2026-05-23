//! server-main — control plane (REST API, registry, command queue, uploads).
//! Phase 1: DB layer wired (1c). REST/auth/queue arrive in 1d+.

mod db;
mod registry;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database_url =
        std::env::var("MP_DATABASE_URL").unwrap_or_else(|_| "sqlite://mp.db".to_string());

    let _pool = db::connect(&database_url).await?;
    println!("server-main: database ready ({database_url}), migrations applied");

    // TODO(1d/1e): bind axum, mount auth + device + controller routes.
    Ok(())
}
