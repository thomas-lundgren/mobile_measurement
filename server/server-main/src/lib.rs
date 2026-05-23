//! server-main — control plane library. Binary entry is `src/main.rs`.

pub mod auth;
pub mod commands;
pub mod db;
pub mod device;
pub mod error;
pub mod registry;
pub mod resolve;
pub mod state;
pub mod util;

pub use state::AppState;

use axum::Router;

/// Build the full application router (auth + device control plane; controller
/// and upload routes mount here in later sub-phases).
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .merge(auth::routes())
        .merge(device::routes())
        .with_state(state)
}
