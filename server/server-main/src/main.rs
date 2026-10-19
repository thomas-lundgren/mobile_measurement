//! server-main binary — control plane (REST API, registry, command queue, uploads).
//! Phase 1: DB (1c) + device/operator auth (1d). Plain HTTP on loopback (TLS is
//! a Phase-5 hardening concern, per the Phase 1 design).

use mp_server_main::{build_router, db, AppState, UploadHold};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database_url =
        std::env::var("MP_DATABASE_URL").unwrap_or_else(|_| "sqlite://mp.db".to_string());
    let jwt_secret =
        std::env::var("MP_JWT_SECRET").unwrap_or_else(|_| "dev-secret-change-me".to_string());
    let server_id = std::env::var("MP_SERVER_ID").unwrap_or_else(|_| "srv_main_01".to_string());
    let bind = std::env::var("MP_BIND").unwrap_or_else(|_| "127.0.0.1:8080".to_string());

    let pool = db::connect(&database_url).await?;
    let mut state = AppState::new(pool, jwt_secret, server_id);
    // Host the UE is told to connect to for data flows (RUN_TEST server_host).
    if let Ok(host) = std::env::var("MP_SERVER_HOST") {
        state = state.with_server_host(host);
    }
    // Internal test-control endpoint; without it POST /tests is rejected.
    if let Ok(url) = std::env::var("MP_TEST_CONTROL_URL") {
        state = state.with_test_control_url(url);
    }
    if let Ok(root) = std::env::var("MP_UPLOAD_ROOT") {
        state = state.with_upload_root(root);
    }
    // Low-overhead UDP time-sync responder (device clock calibration).
    let time_udp_port: u16 = std::env::var("MP_TIME_UDP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8081);
    state = state.with_time_udp_port(time_udp_port);
    tokio::spawn(async move {
        if let Err(e) = mp_server_main::time_sync::run(&format!("0.0.0.0:{time_udp_port}")).await {
            tracing::error!("time-sync responder failed: {e}");
        }
    });

    // Reach responders for the device's reachability check (contracts/packets/reach.md).
    if let Ok(spec) = std::env::var("MP_REACH_LISTEN") {
        let ports = mp_server_main::reach::parse_listen(&spec)?;
        for (proto, addr) in mp_server_main::reach::spawn("0.0.0.0", &ports)? {
            println!("reach responder {proto:?} on {addr}");
        }
    }

    // MP_UPLOAD_HOLD_MIN_FREE_GB: hold mobile uploads until the phone is on
    // the local network, unless its free storage drops below this many GB.
    if let Ok(gb) = std::env::var("MP_UPLOAD_HOLD_MIN_FREE_GB") {
        let gb: f64 = gb.parse()?;
        state = state.with_upload_hold(UploadHold {
            min_free_bytes: (gb * 1e9) as i64,
        });
        println!("mobile uploads held off the local network (unless < {gb} GB free)");
    }

    let app = build_router(state)
        .into_make_service_with_connect_info::<std::net::SocketAddr>();

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    println!("server-main listening on http://{bind}");
    axum::serve(listener, app).await?;
    Ok(())
}
