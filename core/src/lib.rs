//! fieldtest-core — the shared engine (server + Android via JNI).
//!
//! `RustCore` is the Logger's native handle: it drives the UE traffic engine
//! (`ue`) from JSON config and exposes status / sealed-file listing. The logic
//! is pure Rust (unit-testable without a JVM); the `Java_com_measurement_logger_
//! RustCore_*` functions are thin JNI shims over it.

pub mod app_traffic;
pub mod auth;
pub mod aux_log;
pub mod endpoint;
pub mod log_writer;
pub mod packet;
pub mod timestamp;
pub mod udp_timing;
pub mod ue;

use aux_log::{AuxContext, ClockRow, GpsRow, RadioRow};
use jni::objects::{JClass, JString};
use jni::sys::{jint, jlong};
use jni::JNIEnv;
use std::path::PathBuf;
use std::sync::Mutex;
use timestamp::Timestamp;
use ue::{UeEngine, UeTestConfig};

/// Per-test session state: where logs go + buffered low-rate aux rows
/// (gps/clock) pushed from the Java shell. packets are written live by the
/// engine; aux tables are buffered and sealed at stop.
struct Session {
    ctx: AuxContext,
    gps: Vec<GpsRow>,
    clock: Vec<ClockRow>,
    radio: Vec<RadioRow>,
}

/// One native core handle per app session. Holds the running test's engine +
/// its aux-log session.
#[derive(Default)]
pub struct RustCore {
    engine: Mutex<Option<UeEngine>>,
    session: Mutex<Option<Session>>,
    /// (mobile_dir, root_dir) of the most recent test — kept past stop so the
    /// upload manager can still list sealed files.
    dirs: Mutex<Option<(PathBuf, PathBuf)>>,
}

impl RustCore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a test from a `UeTestConfig` JSON blob (the Java shell merges the
    /// `RunTestPayload` + `flow_bindings` into this). Idempotently stops any
    /// prior run first.
    pub fn start_test(&self, config_json: &str) -> Result<(), String> {
        self.stop_test();
        let cfg: UeTestConfig =
            serde_json::from_str(config_json).map_err(|e| format!("bad config: {e}"))?;
        let mobile_dir = PathBuf::from(&cfg.log_dir);
        let root_dir = mobile_dir
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| mobile_dir.clone());
        let ctx = AuxContext {
            device_id: cfg.device_id.clone(),
            session_id: cfg.session_id.clone(),
            test_id: cfg.test_id.clone(),
            node: "mobile".to_string(),
        };
        let engine = UeEngine::start(cfg).map_err(|e| format!("start failed: {e}"))?;
        *self.engine.lock().unwrap() = Some(engine);
        *self.dirs.lock().unwrap() = Some((mobile_dir, root_dir));
        *self.session.lock().unwrap() = Some(Session {
            ctx,
            gps: Vec::new(),
            clock: Vec::new(),
            radio: Vec::new(),
        });
        Ok(())
    }

    /// Buffer a GPS row (JSON) for this test's `gps.parquet`.
    pub fn push_gps(&self, json: &str) -> Result<(), String> {
        let row: GpsRow = serde_json::from_str(json).map_err(|e| format!("bad gps: {e}"))?;
        match &mut *self.session.lock().unwrap() {
            Some(s) => {
                s.gps.push(row);
                Ok(())
            }
            None => Err("no active test".into()),
        }
    }

    /// Buffer a clock-calibration row (JSON) for this test's `clock.parquet`.
    pub fn push_clock(&self, json: &str) -> Result<(), String> {
        let row: ClockRow = serde_json::from_str(json).map_err(|e| format!("bad clock: {e}"))?;
        match &mut *self.session.lock().unwrap() {
            Some(s) => {
                s.clock.push(row);
                Ok(())
            }
            None => Err("no active test".into()),
        }
    }

    /// Buffer a radio metric row (JSON) for this test's `radio.parquet`.
    pub fn push_radio(&self, json: &str) -> Result<(), String> {
        let row: RadioRow = serde_json::from_str(json).map_err(|e| format!("bad radio: {e}"))?;
        match &mut *self.session.lock().unwrap() {
            Some(s) => {
                s.radio.push(row);
                Ok(())
            }
            None => Err("no active test".into()),
        }
    }

    /// Live per-flow counters as JSON (`[FlowSummary]`); `[]` if no test runs.
    pub fn poll_status(&self) -> String {
        match &*self.engine.lock().unwrap() {
            Some(e) => serde_json::to_string(&e.status()).unwrap_or_else(|_| "[]".into()),
            None => "[]".into(),
        }
    }

    /// Stop the test, seal Parquet (packets via the engine; gps/clock here),
    /// return per-flow summaries as JSON. Safe to call when idle (`[]`).
    pub fn stop_test(&self) -> String {
        let summary = match self.engine.lock().unwrap().take() {
            Some(e) => serde_json::to_string(&e.stop()).unwrap_or_else(|_| "[]".into()),
            None => "[]".into(),
        };
        let sess = self.session.lock().unwrap().take();
        let dirs = self.dirs.lock().unwrap().clone();
        if let (Some(s), Some((mobile, root))) = (sess, dirs) {
            if !s.gps.is_empty() {
                let _ = aux_log::write_gps(&root.join("gps.parquet"), &s.ctx, &s.gps);
            }
            if !s.clock.is_empty() {
                let _ = aux_log::write_clock(&mobile.join("clock.parquet"), &s.ctx, &s.clock);
            }
            if !s.radio.is_empty() {
                let _ = aux_log::write_radio(&root.join("radio.parquet"), &s.ctx, &s.radio);
            }
        }
        summary
    }

    /// JSON array of sealed log files the upload manager may read. Scans by
    /// known filename so it never picks up unrelated files.
    pub fn sealed_files(&self) -> String {
        let dirs = self.dirs.lock().unwrap().clone();
        let mut files: Vec<String> = Vec::new();
        if let Some((mobile, root)) = dirs {
            if let Ok(rd) = std::fs::read_dir(&mobile) {
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    if (name.starts_with("packets") || name == "clock.parquet")
                        && name.ends_with(".parquet")
                    {
                        files.push(e.path().to_string_lossy().to_string());
                    }
                }
            }
            for f in ["gps.parquet", "radio.parquet"] {
                let p = root.join(f);
                if p.exists() {
                    files.push(p.to_string_lossy().to_string());
                }
            }
        }
        files.sort();
        serde_json::to_string(&files).unwrap_or_else(|_| "[]".into())
    }
}

// ---------------------------------------------------------------------------
// JNI shims — static natives on `com.measurement.logger.RustCore`. The handle
// (a *RustCore) is passed explicitly as a jlong.
// ---------------------------------------------------------------------------

fn jstr(env: &mut JNIEnv, js: &JString) -> Option<String> {
    if js.is_null() {
        return None;
    }
    env.get_string(js)
        .ok()
        .and_then(|s| s.to_str().ok().map(str::to_owned))
}

/// SAFETY: `handle` must be a live pointer returned by `coreNew` and not yet
/// destroyed.
unsafe fn core<'a>(handle: jlong) -> &'a RustCore {
    &*(handle as *const RustCore)
}

#[no_mangle]
pub extern "system" fn Java_com_measurement_logger_RustCore_coreNew(
    _env: JNIEnv,
    _class: JClass,
) -> jlong {
    Box::into_raw(Box::new(RustCore::new())) as jlong
}

#[no_mangle]
pub extern "system" fn Java_com_measurement_logger_RustCore_startTest(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    config: JString,
) -> jint {
    if handle == 0 {
        return -1;
    }
    let cfg = match jstr(&mut env, &config) {
        Some(s) => s,
        None => return -1,
    };
    match unsafe { core(handle) }.start_test(&cfg) {
        Ok(()) => 0,
        Err(e) => {
            let _ = env.throw_new("java/lang/RuntimeException", e);
            -1
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_com_measurement_logger_RustCore_pushGps(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    row: JString,
) -> jint {
    push_row(&mut env, handle, &row, |c, j| c.push_gps(j))
}

#[no_mangle]
pub extern "system" fn Java_com_measurement_logger_RustCore_pushClock(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    row: JString,
) -> jint {
    push_row(&mut env, handle, &row, |c, j| c.push_clock(j))
}

#[no_mangle]
pub extern "system" fn Java_com_measurement_logger_RustCore_pushRadio(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    row: JString,
) -> jint {
    push_row(&mut env, handle, &row, |c, j| c.push_radio(j))
}

fn push_row(
    env: &mut JNIEnv,
    handle: jlong,
    row: &JString,
    f: impl FnOnce(&RustCore, &str) -> Result<(), String>,
) -> jint {
    if handle == 0 {
        return -1;
    }
    let json = match jstr(env, row) {
        Some(s) => s,
        None => return -1,
    };
    match f(unsafe { core(handle) }, &json) {
        Ok(()) => 0,
        Err(e) => {
            let _ = env.throw_new("java/lang/RuntimeException", e);
            -1
        }
    }
}

#[no_mangle]
pub extern "system" fn Java_com_measurement_logger_RustCore_pollStatus<'a>(
    env: JNIEnv<'a>,
    _class: JClass<'a>,
    handle: jlong,
) -> JString<'a> {
    let s = if handle == 0 {
        "[]".to_string()
    } else {
        unsafe { core(handle) }.poll_status()
    };
    env.new_string(s).expect("JNI new_string")
}

#[no_mangle]
pub extern "system" fn Java_com_measurement_logger_RustCore_stopTest<'a>(
    env: JNIEnv<'a>,
    _class: JClass<'a>,
    handle: jlong,
) -> JString<'a> {
    let s = if handle == 0 {
        "[]".to_string()
    } else {
        unsafe { core(handle) }.stop_test()
    };
    env.new_string(s).expect("JNI new_string")
}

#[no_mangle]
pub extern "system" fn Java_com_measurement_logger_RustCore_sealedFiles<'a>(
    env: JNIEnv<'a>,
    _class: JClass<'a>,
    handle: jlong,
) -> JString<'a> {
    let s = if handle == 0 {
        "[]".to_string()
    } else {
        unsafe { core(handle) }.sealed_files()
    };
    env.new_string(s).expect("JNI new_string")
}

#[no_mangle]
pub extern "system" fn Java_com_measurement_logger_RustCore_sampleClock<'a>(
    env: JNIEnv<'a>,
    _class: JClass<'a>,
) -> JString<'a> {
    let json = serde_json::to_string(&Timestamp::now()).unwrap_or_else(|_| "{}".into());
    env.new_string(json).expect("JNI new_string")
}

#[no_mangle]
pub extern "system" fn Java_com_measurement_logger_RustCore_destroy(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if handle != 0 {
        // SAFETY: reclaims the Box leaked in coreNew; called once.
        let _ = unsafe { Box::from_raw(handle as *mut RustCore) };
    }
}
