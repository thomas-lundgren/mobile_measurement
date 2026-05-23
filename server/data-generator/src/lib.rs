//! data-generator — server-side traffic engine, driven by the internal control
//! API (`contracts/internal/control_api.md`). 1f: probe echo. realtime/bulk
//! land in 1j. Loopback-only, no auth.

pub mod flow;

use std::collections::HashMap;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use fieldtest_core::log_writer::{packet_channel, spawn_packet_writer, LogContext};
use mp_contracts::{Flow, GeneratorCounters, GeneratorState, InternalGeneratorPrepare, InternalGeneratorStatus};

use flow::{spawn_probe_echo, Counters};

const RING_CAPACITY: usize = 8192;
const SEGMENT_ROWS: usize = 1000;
const READ_TIMEOUT: Duration = Duration::from_millis(200);

struct GeneratorEntry {
    test_id: String,
    flow_id: u32,
    bound_port: u16,
    state: Arc<Mutex<GeneratorState>>,
    counters: Arc<Counters>,
    dropped: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Clone, Default)]
pub struct AppState {
    manager: Arc<Mutex<HashMap<String, Arc<GeneratorEntry>>>>,
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/internal/generators", post(prepare))
        .route("/internal/generators/{generator_id}", get(status))
        .route("/internal/generators/{generator_id}/stop", post(stop))
        .with_state(state)
}

fn status_of(generator_id: &str, e: &GeneratorEntry) -> InternalGeneratorStatus {
    InternalGeneratorStatus {
        generator_id: generator_id.to_string(),
        test_id: e.test_id.clone(),
        flow_id: e.flow_id,
        state: *e.state.lock().unwrap(),
        bound_port: Some(e.bound_port),
        counters: GeneratorCounters {
            packets_sent: e.counters.sent.load(Ordering::Relaxed),
            packets_received: e.counters.received.load(Ordering::Relaxed),
            bytes_sent: e.counters.bytes_sent.load(Ordering::Relaxed),
            bytes_received: e.counters.bytes_received.load(Ordering::Relaxed),
            dropped_rows: e.dropped.load(Ordering::Relaxed),
        },
        error: None,
    }
}

async fn prepare(
    State(st): State<AppState>,
    Json(req): Json<InternalGeneratorPrepare>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !matches!(req.flow, Flow::Probe { .. }) {
        return Err((StatusCode::BAD_REQUEST, "only probe flows supported (1f)".into()));
    }
    let flow_id = req.flow.flow_id();

    let socket = UdpSocket::bind(("0.0.0.0", req.server_port))
        .map_err(|e| (StatusCode::CONFLICT, format!("bind {}: {e}", req.server_port)))?;
    socket.set_read_timeout(Some(READ_TIMEOUT)).map_err(internal)?;
    let bound_port = socket.local_addr().map_err(internal)?.port();

    let ctx = LogContext {
        device_id: req.device_id,
        session_id: req.session_id,
        test_id: req.test_id.clone(),
        node: "server".to_string(),
    };
    let (sink, rx, dropped) = packet_channel(RING_CAPACITY);
    let writer = spawn_packet_writer(rx, &req.node_log_dir, SEGMENT_ROWS, ctx).map_err(internal)?;

    let state = Arc::new(Mutex::new(GeneratorState::Ready));
    let counters = Arc::new(Counters::default());
    let stop = Arc::new(AtomicBool::new(false));
    let handle = spawn_probe_echo(socket, sink, writer, flow_id, state.clone(), counters.clone(), stop.clone());

    let generator_id = format!("gen_{}", ulid::Ulid::new());
    let entry = Arc::new(GeneratorEntry {
        test_id: req.test_id,
        flow_id,
        bound_port,
        state,
        counters,
        dropped,
        stop,
        handle: Mutex::new(Some(handle)),
    });
    st.manager.lock().unwrap().insert(generator_id.clone(), entry);

    Ok(Json(serde_json::json!({ "generator_id": generator_id })))
}

async fn status(
    State(st): State<AppState>,
    Path(generator_id): Path<String>,
) -> Result<Json<InternalGeneratorStatus>, (StatusCode, String)> {
    let entry = st.manager.lock().unwrap().get(&generator_id).cloned();
    match entry {
        Some(e) => Ok(Json(status_of(&generator_id, &e))),
        None => Err((StatusCode::NOT_FOUND, "unknown generator".into())),
    }
}

async fn stop(
    State(st): State<AppState>,
    Path(generator_id): Path<String>,
) -> Result<Json<InternalGeneratorStatus>, (StatusCode, String)> {
    let entry = st.manager.lock().unwrap().get(&generator_id).cloned();
    let Some(e) = entry else {
        return Err((StatusCode::NOT_FOUND, "unknown generator".into()));
    };
    e.stop.store(true, Ordering::Release);
    let handle = e.handle.lock().unwrap().take();
    if let Some(h) = handle {
        // flow thread finalizes the writer; brief blocking join is acceptable.
        let _ = tokio::task::spawn_blocking(move || h.join()).await;
    }
    Ok(Json(status_of(&generator_id, &e)))
}

fn internal<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}
