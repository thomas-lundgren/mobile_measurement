//! Shared application state (cheap to clone — pool + secret are Arc-backed).

use sqlx::SqlitePool;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    /// HS256 signing secret for operator JWTs.
    pub jwt_secret: Arc<String>,
    /// Stable server identity, bound into the auth challenge blob.
    pub server_id: String,
}

impl AppState {
    pub fn new(pool: SqlitePool, jwt_secret: impl Into<String>, server_id: impl Into<String>) -> Self {
        Self {
            pool,
            jwt_secret: Arc::new(jwt_secret.into()),
            server_id: server_id.into(),
        }
    }
}
