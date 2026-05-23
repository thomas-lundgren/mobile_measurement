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
    /// Address advertised to devices for them to connect to (server_host).
    pub server_host: String,
    /// test-control internal API base URL (None until configured).
    pub test_control_url: Arc<Option<String>>,
}

impl AppState {
    pub fn new(
        pool: SqlitePool,
        jwt_secret: impl Into<String>,
        server_id: impl Into<String>,
    ) -> Self {
        Self {
            pool,
            jwt_secret: Arc::new(jwt_secret.into()),
            server_id: server_id.into(),
            upload_root: Arc::new(std::env::temp_dir().join("mp-uploads")),
            server_host: "127.0.0.1".to_string(),
            test_control_url: Arc::new(None),
        }
    }

    pub fn with_upload_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.upload_root = Arc::new(root.into());
        self
    }

    pub fn with_server_host(mut self, host: impl Into<String>) -> Self {
        self.server_host = host.into();
        self
    }

    pub fn with_test_control_url(mut self, url: impl Into<String>) -> Self {
        self.test_control_url = Arc::new(Some(url.into()));
        self
    }
}
