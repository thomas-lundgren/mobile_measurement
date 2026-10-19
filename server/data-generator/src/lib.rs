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
use fieldtest_core::aux_log::{write_events, AuxContext, EventRow};
use fieldtest_core::log_writer::{packet_channel, spawn_packet_writer, LogContext};
use mp_contracts::{
    Flow, GeneratorCounters, GeneratorState, InternalGeneratorPrepare, InternalGeneratorStatus,
};

use flow::Counters;

const RING_CAPACITY: usize = 8192;
const SEGMENT_ROWS: usize = 1000;
const READ_TIMEOUT: Duration = Duration::from_millis(200);

fn bind_udp(port: u16) -> Result<UdpSocket, (StatusCode, String)> {
    let s = UdpSocket::bind(("0.0.0.0", port))
        .map_err(|e| (StatusCode::CONFLICT, format!("bind udp {port}: {e}")))?;
    s.set_read_timeout(Some(READ_TIMEOUT)).map_err(internal)?;
    Ok(s)
}

struct GeneratorEntry {
    test_id: String,
    flow_id: u32,
    /// Where this flow's server logs go, and their join keys (for events).
    log_dir: std::path::PathBuf,
    log_ctx: AuxContext,
    protocol: &'static str,
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
    let flow_id = req.flow.flow_id();

    let ctx = LogContext {
        device_id: req.device_id.clone(),
        session_id: req.session_id.clone(),
        test_id: req.test_id.clone(),
        node: "server".to_string(),
    };
    let (sink, rx, dropped) = packet_channel(RING_CAPACITY);
    // One writer per flow into the shared server/ dir → flow-scoped filename
    // stem so concurrent flows don't collide on packets_NNNN.parquet.
    let writer = spawn_packet_writer(
        rx,
        &req.node_log_dir,
        SEGMENT_ROWS,
        ctx,
        &format!("packets_f{flow_id}"),
    )
    .map_err(internal)?;

    let state = Arc::new(Mutex::new(GeneratorState::Ready));
    let counters = Arc::new(Counters::default());
    let stop = Arc::new(AtomicBool::new(false));

    // bind the right socket per flow type and spawn its real-time thread
    let (handle, bound_port) = match &req.flow {
        Flow::Probe { .. } => {
            let socket = bind_udp(req.server_port)?;
            let port = socket.local_addr().map_err(internal)?.port();
            (
                flow::spawn_probe_echo(
                    socket,
                    sink,
                    writer,
                    flow_id,
                    state.clone(),
                    counters.clone(),
                    stop.clone(),
                ),
                port,
            )
        }
        Flow::RealtimeUdp {
            direction,
            rate_pps,
            packet_size_bytes,
            ..
        } => {
            let socket = bind_udp(req.server_port)?;
            let port = socket.local_addr().map_err(internal)?.port();
            let h = flow::spawn_realtime_udp(
                socket,
                sink,
                writer,
                flow_id,
                *direction,
                *rate_pps,
                *packet_size_bytes as usize,
                state.clone(),
                counters.clone(),
                stop.clone(),
            );
            (h, port)
        }
        Flow::BulkTcp {
            direction,
            target_bytes,
            ..
        } => {
            let listener =
                std::net::TcpListener::bind(("0.0.0.0", req.server_port)).map_err(|e| {
                    (
                        StatusCode::CONFLICT,
                        format!("bind tcp {}: {e}", req.server_port),
                    )
                })?;
            let port = listener.local_addr().map_err(internal)?.port();
            let h = flow::spawn_bulk_tcp(
                listener,
                sink,
                writer,
                flow_id,
                *direction,
                target_bytes.map(|t| t as u64),
                state.clone(),
                counters.clone(),
                stop.clone(),
            );
            (h, port)
        }
        // gateway_ping has no server endpoint — the generator should never receive this.
        Flow::GatewayPing { .. } => {
            return Err((
                StatusCode::BAD_REQUEST,
                "gateway_ping flows have no server generator".to_string(),
            ))
        }
        Flow::DscpProbe { .. } => {
            let socket = bind_udp(req.server_port)?;
            let port = socket.local_addr().map_err(internal)?.port();
            (
                flow::spawn_dscp_probe_echo(
                    socket,
                    sink,
                    writer,
                    flow_id,
                    state.clone(),
                    counters.clone(),
                    stop.clone(),
                ),
                port,
            )
        }
    };

    let protocol = match &req.flow {
        Flow::BulkTcp { .. } => "TCP",
        _ => "UDP",
    };
    let generator_id = format!("gen_{}", ulid::Ulid::new());
    let entry = Arc::new(GeneratorEntry {
        log_dir: std::path::PathBuf::from(&req.node_log_dir),
        log_ctx: AuxContext {
            device_id: req.device_id.clone(),
            session_id: req.session_id.clone(),
            test_id: req.test_id.clone(),
            node: "server".to_string(),
        },
        protocol,
        test_id: req.test_id,
        flow_id,
        bound_port,
        state,
        counters,
        dropped,
        stop,
        handle: Mutex::new(Some(handle)),
    });
    st.manager
        .lock()
        .unwrap()
        .insert(generator_id.clone(), entry);

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
    write_peer_event(&e);
    Ok(Json(status_of(&generator_id, &e)))
}

/// CLIENT_CONNECTED with the UE's address as the server saw it (after NAT),
/// so every test records which network it actually ran over. Written once
/// (the peer is taken), best-effort: a failure only loses the event.
fn write_peer_event(e: &GeneratorEntry) {
    let Some((ts, peer)) = e.counters.peer.lock().unwrap().take() else {
        return;
    };
    let row = EventRow {
        mono_ns: ts.mono_ns,
        utc_ns: ts.utc_ns,
        event_type: "CLIENT_CONNECTED".into(),
        flow_id: Some(e.flow_id),
        event_metadata: Some(
            serde_json::json!({
                "peer_ip": peer.ip().to_string(),
                "peer_port": peer.port(),
                "protocol": e.protocol,
            })
            .to_string(),
        ),
    };
    let path = e.log_dir.join(format!("events_f{}.parquet", e.flow_id));
    if let Err(err) = write_events(&path, &e.log_ctx, &[row]) {
        tracing::warn!(flow_id = e.flow_id, %err, "could not write CLIENT_CONNECTED event");
    }
}

fn internal<E: std::fmt::Display>(e: E) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}
