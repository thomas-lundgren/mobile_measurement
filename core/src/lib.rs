//! fieldtest-core — the shared engine (server + Android via JNI).
//!
//! `RustCore` is the Logger's native handle: it drives the UE traffic engine
//! (`ue`) from JSON config and exposes status / sealed-file listing. The logic
//! is pure Rust (unit-testable without a JVM); the `Java_com_measurement_logger_
//! RustCore_*` functions are thin JNI shims over it.

pub mod auth;
pub mod log_writer;
pub mod packet;
pub mod timestamp;
pub mod ue;

use jni::objects::{JClass, JString};
use jni::sys::{jint, jlong};
use jni::JNIEnv;
use std::sync::Mutex;
use timestamp::Timestamp;
use ue::{UeEngine, UeTestConfig};

/// One native core handle per app session. Holds the running test's engine.
#[derive(Default)]
pub struct RustCore {
    engine: Mutex<Option<UeEngine>>,
    log_dir: Mutex<Option<String>>,
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
        let log_dir = cfg.log_dir.clone();
        let engine = UeEngine::start(cfg).map_err(|e| format!("start failed: {e}"))?;
        *self.engine.lock().unwrap() = Some(engine);
        *self.log_dir.lock().unwrap() = Some(log_dir);
        Ok(())
    }

    /// Live per-flow counters as JSON (`[FlowSummary]`); `[]` if no test runs.
    pub fn poll_status(&self) -> String {
        match &*self.engine.lock().unwrap() {
            Some(e) => serde_json::to_string(&e.status()).unwrap_or_else(|_| "[]".into()),
            None => "[]".into(),
        }
    }

    /// Stop the test, seal Parquet, return per-flow summaries as JSON. Safe to
    /// call with no test running (returns `[]`).
    pub fn stop_test(&self) -> String {
        let engine = self.engine.lock().unwrap().take();
        match engine {
            Some(e) => serde_json::to_string(&e.stop()).unwrap_or_else(|_| "[]".into()),
            None => "[]".into(),
        }
    }

    /// JSON array of sealed `*.parquet` paths the upload manager may read.
    pub fn sealed_files(&self) -> String {
        let dir = self.log_dir.lock().unwrap().clone();
        let mut files: Vec<String> = Vec::new();
        if let Some(d) = dir {
            if let Ok(rd) = std::fs::read_dir(&d) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().map(|x| x == "parquet").unwrap_or(false) {
                        if let Some(s) = p.to_str() {
                            files.push(s.to_string());
                        }
                    }
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
