//! server-main — control plane library. Binary entry is `src/main.rs`.

pub mod auth;
pub mod db;
pub mod error;
pub mod registry;
pub mod state;
pub mod util;

pub use state::AppState;

use axum::Router;

/// Build the full application router (device + operator auth in 1d; device,
/// controller, and upload routes mount here in 1e+).
pub fn build_router(state: AppState) -> Router {
    Router::new().merge(auth::routes()).with_state(state)
}
