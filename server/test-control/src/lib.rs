//! test-control — per-device test state machine, port allocation, and
//! generator coordination. Exposes the internal API to server-main; acts as a
//! client to data-generator(s). Loopback-only, no auth.
//! (`contracts/internal/control_api.md`)

pub mod genclient;
pub mod ports;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use mp_contracts::{
    Flow, FlowBinding, GeneratorRef, GeneratorState, InternalGeneratorPrepare, InternalStartTest,
    InternalTestStatus, TestControlState, TestResult,
};

use genclient::GenClient;
use ports::PortAllocator;

const READY_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

pub struct Config {
    pub generator_base_url: String,
    pub log_root: PathBuf,
    pub port_base: u16,
    pub port_count: u16,
}

struct TestEntry {
    test_id: String,
    device_id: String,
    state: TestControlState,
    flow_bindings: Vec<FlowBinding>,
    generators: Vec<GeneratorRef>,
    result: Option<TestResult>,
    error: Option<String>,
}

struct Inner {
    cfg: Config,
    gen: GenClient,
    ports: Mutex<PortAllocator>,
    tests: Mutex<HashMap<String, Arc<Mutex<TestEntry>>>>,
}

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

impl AppState {
    pub fn new(cfg: Config) -> Self {
        let gen = GenClient::new(cfg.generator_base_url.clone());
        let ports = Mutex::new(PortAllocator::new(cfg.port_base, cfg.port_count));
        AppState(Arc::new(Inner {
            cfg,
            gen,
            ports,
            tests: Mutex::new(HashMap::new()),
        }))
    }

    fn get(&self, test_id: &str) -> Option<Arc<Mutex<TestEntry>>> {
        self.0.tests.lock().unwrap().get(test_id).cloned()
    }
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/internal/tests", post(start))
        .route("/internal/tests/{test_id}", get(status))
        .route("/internal/tests/{test_id}/stop", post(stop))
        .with_state(state)
}

fn snapshot(e: &TestEntry) -> InternalTestStatus {
    InternalTestStatus {
        test_id: e.test_id.clone(),
        device_id: e.device_id.clone(),
        state: e.state,
        flow_bindings: e.flow_bindings.clone(),
        generators: e.generators.clone(),
        result: e.result,
        error: e.error.clone(),
    }
}

async fn start(
    State(st): State<AppState>,
    Json(req): Json<InternalStartTest>,
) -> Result<StatusCode, (StatusCode, String)> {
    if req.plan.flows.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "plan has no flows".into()));
    }
    let entry = Arc::new(Mutex::new(TestEntry {
        test_id: req.test_id.clone(),
        device_id: req.device_id.clone(),
        state: TestControlState::Prepare,
        flow_bindings: Vec::new(),
        generators: Vec::new(),
        result: None,
        error: None,
    }));
    st.0.tests
        .lock()
        .unwrap()
        .insert(req.test_id.clone(), entry);
    tokio::spawn(drive(st.clone(), req));
    Ok(StatusCode::ACCEPTED)
}

/// Background: allocate ports, prepare a generator per flow, wait for READY.
async fn drive(st: AppState, req: InternalStartTest) {
    let entry = match st.get(&req.test_id) {
        Some(e) => e,
        None => return,
    };

    let mut allocated: Vec<u16> = Vec::new();
    let mut bindings: Vec<FlowBinding> = Vec::new();
    let mut generators: Vec<GeneratorRef> = Vec::new();
    let node_log_dir =
        st.0.cfg
            .log_root
            .join(&req.session_id)
            .join(&req.test_id)
            .join("server")
            .to_string_lossy()
            .to_string();

    for flow in &req.plan.flows {
        let flow_id = flow.flow_id();

        // gateway_ping is a device-local measurement — no server endpoint, no generator.
        if matches!(flow, Flow::GatewayPing { .. }) {
            bindings.push(FlowBinding { flow_id, server_port: None });
            continue;
        }

        let port = match st.0.ports.lock().unwrap().allocate() {
            Some(p) => p,
            None => return fail(&entry, &st, &allocated, "port range exhausted"),
        };
        allocated.push(port);

        let prep = InternalGeneratorPrepare {
            test_id: req.test_id.clone(),
            device_id: req.device_id.clone(),
            session_id: req.session_id.clone(),
            flow: flow.clone(),
            server_port: port,
            node_log_dir: node_log_dir.clone(),
        };
        match st.0.gen.prepare(&prep).await {
            Ok(gid) => {
                bindings.push(FlowBinding {
                    flow_id,
                    server_port: Some(port),
                });
                generators.push(GeneratorRef {
                    flow_id,
                    generator_id: gid,
                    state: GeneratorState::Initializing,
                });
            }
            Err(e) => {
                return fail(
                    &entry,
                    &st,
                    &allocated,
                    &format!("generator prepare failed: {e}"),
                )
            }
        }
    }

    {
        let mut e = entry.lock().unwrap();
        e.flow_bindings = bindings;
        e.generators = generators.clone();
        e.state = TestControlState::WaitingForGenerator;
    }

    // wait for all generators READY (or RUNNING if a client beat us)
    let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
    loop {
        let mut all_ready = true;
        for g in generators.iter_mut() {
            if let Ok(s) = st.0.gen.status(&g.generator_id).await {
                g.state = s.state;
            }
            if !matches!(
                g.state,
                GeneratorState::Ready | GeneratorState::Running | GeneratorState::WaitingForClient
            ) {
                all_ready = false;
            }
        }
        {
            let mut e = entry.lock().unwrap();
            e.generators = generators.clone();
        }
        if all_ready {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            return fail(&entry, &st, &allocated, "generators did not reach READY");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    entry.lock().unwrap().state = TestControlState::WaitingForClient;
}

fn fail(entry: &Arc<Mutex<TestEntry>>, st: &AppState, allocated: &[u16], msg: &str) {
    let mut ports = st.0.ports.lock().unwrap();
    for p in allocated {
        ports.release(*p);
    }
    let mut e = entry.lock().unwrap();
    e.state = TestControlState::Failed;
    e.result = Some(TestResult::Failed);
    e.error = Some(msg.to_string());
}

async fn status(
    State(st): State<AppState>,
    Path(test_id): Path<String>,
) -> Result<Json<InternalTestStatus>, (StatusCode, String)> {
    let entry = st
        .get(&test_id)
        .ok_or((StatusCode::NOT_FOUND, "unknown test".into()))?;

    // refresh generator states; promote WAITING_FOR_CLIENT → RUNNING if any
    // generator has seen the client.
    let gens: Vec<GeneratorRef> = entry.lock().unwrap().generators.clone();
    let mut refreshed = Vec::with_capacity(gens.len());
    let mut any_running = false;
    for mut g in gens {
        if let Ok(s) = st.0.gen.status(&g.generator_id).await {
            g.state = s.state;
        }
        if g.state == GeneratorState::Running {
            any_running = true;
        }
        refreshed.push(g);
    }
    {
        let mut e = entry.lock().unwrap();
        e.generators = refreshed;
        if any_running && e.state == TestControlState::WaitingForClient {
            e.state = TestControlState::Running;
        }
    }
    let resp = snapshot(&entry.lock().unwrap());
    Ok(Json(resp))
}

async fn stop(
    State(st): State<AppState>,
    Path(test_id): Path<String>,
) -> Result<Json<InternalTestStatus>, (StatusCode, String)> {
    let entry = st
        .get(&test_id)
        .ok_or((StatusCode::NOT_FOUND, "unknown test".into()))?;
    // Idempotent: server-main stops on RUN_TEST ack and again on upload
    // complete. A repeat must not re-release ports another test may now hold.
    {
        let mut e = entry.lock().unwrap();
        if matches!(e.state, TestControlState::Done | TestControlState::Failed) {
            return Ok(Json(snapshot(&e)));
        }
        e.state = TestControlState::Stopping;
    }

    let (gens, bindings): (Vec<GeneratorRef>, Vec<FlowBinding>) = {
        let e = entry.lock().unwrap();
        (e.generators.clone(), e.flow_bindings.clone())
    };
    for g in &gens {
        let _ = st.0.gen.stop(&g.generator_id).await;
    }
    {
        let mut ports = st.0.ports.lock().unwrap();
        for b in &bindings {
            if let Some(p) = b.server_port {
                ports.release(p);
            }
        }
    }
    {
        let mut e = entry.lock().unwrap();
        for g in e.generators.iter_mut() {
            g.state = GeneratorState::Stopped;
        }
        e.state = TestControlState::Done;
        e.result = Some(TestResult::Completed);
    }
    let resp = snapshot(&entry.lock().unwrap());
    Ok(Json(resp))
}
