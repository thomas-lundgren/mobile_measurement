//! Shared application state (cheap to clone — Arc-backed).

use sqlx::SqlitePool;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    /// HS256 signing secret for operator JWTs.
    pub jwt_secret: Arc<String>,
    /// Stable server identity, bound into the auth challenge blob.
    pub server_id: String,
    /// Root under which uploaded test trees are assembled (session/test/...).
    pub upload_root: Arc<PathBuf>,
}

impl AppState {
    pub fn new(pool: SqlitePool, jwt_secret: impl Into<String>, server_id: impl Into<String>) -> Self {
        Self {
            pool,
            jwt_secret: Arc::new(jwt_secret.into()),
            server_id: server_id.into(),
            upload_root: Arc::new(std::env::temp_dir().join("mp-uploads")),
        }
    }

    pub fn with_upload_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.upload_root = Arc::new(root.into());
        self
    }
}
