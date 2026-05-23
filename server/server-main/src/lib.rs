//! server-main — control plane library. Binary entry is `src/main.rs`.

pub mod auth;
pub mod commands;
pub mod db;
pub mod device;
pub mod error;
pub mod registry;
pub mod resolve;
pub mod state;
pub mod uploads;
pub mod util;

pub use state::AppState;

use axum::Router;

/// Build the full application router (auth + device control plane + uploads;
/// controller routes mount here in a later sub-phase).
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .merge(auth::routes())
        .merge(device::routes())
        .merge(uploads::routes())
        .with_state(state)
}
