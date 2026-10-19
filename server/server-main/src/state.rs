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
    /// UDP port of the low-overhead time-sync responder, advertised via /time.
    pub time_udp_port: u16,
    /// Hold mobile uploads that arrive over the mobile network (see
    /// `uploads::upload_held`); None uploads over any network.
    pub upload_hold: Option<UploadHold>,
}

/// Mobile uploads are accepted only from the local network (home Wi-Fi), or
/// from anywhere once the device reports less free storage than this.
#[derive(Clone, Copy, Debug)]
pub struct UploadHold {
    pub min_free_bytes: i64,
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
            time_udp_port: 8081,
            upload_hold: None,
        }
    }

    pub fn with_time_udp_port(mut self, port: u16) -> Self {
        self.time_udp_port = port;
        self
    }

    pub fn with_upload_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.upload_root = Arc::new(root.into());
        self
    }

    pub fn with_server_host(mut self, host: impl Into<String>) -> Self {
        self.server_host = host.into();
        self
    }

    pub fn with_upload_hold(mut self, hold: UploadHold) -> Self {
        self.upload_hold = Some(hold);
        self
    }

    pub fn with_test_control_url(mut self, url: impl Into<String>) -> Self {
        self.test_control_url = Arc::new(Some(url.into()));
        self
    }
}
