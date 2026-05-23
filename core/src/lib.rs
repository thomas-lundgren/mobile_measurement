pub mod log_writer;
pub mod packet;
pub mod timestamp;

use jni::objects::JString;
use jni::strings::JavaStr;
use jni::sys::jlong;
use jni::JNIEnv;
use log_writer::{Direction, PacketRow, SegmentedPacketWriter};
use packet::ProbePacket;
use serde::{Deserialize, Serialize};
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use timestamp::Timestamp;

#[derive(Debug, Clone, Deserialize)]
pub struct CoreConfig {
    pub node_id: String,
    pub log_dir: String,
    #[serde(default = "default_segment_rows")]
    pub segment_rows: usize,
}

fn default_segment_rows() -> usize {
    1000
}

#[derive(Debug, Clone, Deserialize)]
pub struct FlowConfig {
    pub flow_id: u32,
    pub server_host: String,
    pub server_port: u16,
    pub local_port: Option<u16>,
    #[serde(default = "default_rate_pps")]
    pub rate_pps: u32,
    #[serde(default = "default_packet_size")]
    pub packet_size: usize,
    #[serde(default = "default_duration_sec")]
    pub duration_sec: u64,
}

fn default_rate_pps() -> u32 {
    10
}
fn default_packet_size() -> usize {
    64
}
fn default_duration_sec() -> u64 {
    60
}

#[derive(Debug, Default, Serialize, Clone)]
pub struct FlowStats {
    pub packets_sent: u64,
    pub packets_received: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub last_seq_sent: u64,
    pub last_seq_recv: u64,
    pub errors: Vec<String>,
}

pub struct CoreState {
    _config: CoreConfig,
    writer: Mutex<Option<SegmentedPacketWriter>>,
    stop_flag: Arc<AtomicBool>,
    stats: Arc<Mutex<FlowStats>>,
    flow_handle: Mutex<Option<JoinHandle<()>>>,
    next_seq: AtomicU64,
}

impl CoreState {
    pub fn new(config: CoreConfig) -> std::io::Result<Self> {
        let writer =
            SegmentedPacketWriter::new(&config.log_dir, config.segment_rows)?;
        Ok(Self {
            _config: config,
            writer: Mutex::new(Some(writer)),
            stop_flag: Arc::new(AtomicBool::new(false)),
            stats: Arc::new(Mutex::new(FlowStats::default())),
            flow_handle: Mutex::new(None),
            next_seq: AtomicU64::new(1),
        })
    }

    pub fn start_flow(&self, flow: FlowConfig) -> std::io::Result<()> {
        // Ensure any prior flow is stopped.
        self.stop();

        self.stop_flag.store(false, Ordering::SeqCst);
        {
            let mut st = self.stats.lock().unwrap();
            *st = FlowStats::default();
        }

        let local_addr = format!(
            "0.0.0.0:{}",
            flow.local_port.unwrap_or(0)
        );
        let socket = UdpSocket::bind(&local_addr)?;
        socket.set_nonblocking(false)?;
        socket.set_read_timeout(Some(Duration::from_millis(100)))?;
        let server_addr = format!("{}:{}", flow.server_host, flow.server_port);
        socket.connect(&server_addr)?;

        let period = Duration::from_secs_f64(1.0 / f64::from(flow.rate_pps.max(1)));
        let payload = vec![0u8; flow.packet_size];
        let stop = Arc::clone(&self.stop_flag);
        let stats = Arc::clone(&self.stats);
        let writer = self.writer.lock().unwrap().take();
        let writer = Mutex::new(writer);
        let _next_seq = &self.next_seq;
        let flow_id = flow.flow_id;
        let duration = Duration::from_secs(flow.duration_sec);

        let handle = thread::spawn(move || {
            let start = std::time::Instant::now();
            let mut local_seq: u64 = 1;
            let mut recv_buf = [0u8; 2048];
            let _payload_size = payload.len();

            while !stop.load(Ordering::SeqCst) && start.elapsed() < duration {
                let seq = local_seq;
                local_seq += 1;

                let ts = Timestamp::now();
                let pkt = ProbePacket {
                    flow_id,
                    sequence_number: seq,
                    send_mono_ns: ts.mono_ns,
                };
                let bytes = pkt.to_bytes();
                if let Err(e) = socket.send(&bytes[..]) {
                    let mut s = stats.lock().unwrap();
                    s.errors.push(format!("send error: {}", e));
                    continue;
                }

                {
                    let mut s = stats.lock().unwrap();
                    s.packets_sent += 1;
                    s.bytes_sent += bytes.len() as u64;
                    s.last_seq_sent = seq;
                }

                if let Ok(n) = socket.recv(&mut recv_buf) {
                    if let Ok(echoed) = ProbePacket::decode(&recv_buf[..n]) {
                        let recv_ts = Timestamp::now();
                        {
                            let mut s = stats.lock().unwrap();
                            s.packets_received += 1;
                            s.bytes_received += n as u64;
                            s.last_seq_recv = echoed.sequence_number;
                        }
                        if let Ok(mut w) = writer.lock() {
                            if let Some(ref mut wr) = *w {
                                let _ = wr.append(&PacketRow {
                                    flow_id,
                                    sequence_number: echoed.sequence_number,
                                    direction: Direction::Received,
                                    timestamp: recv_ts,
                                    payload_size: n as u32,
                                });
                            }
                        }
                    }
                }

                if let Ok(mut w) = writer.lock() {
                    if let Some(ref mut wr) = *w {
                        let _ = wr.append(&PacketRow {
                            flow_id,
                            sequence_number: seq,
                            direction: Direction::Sent,
                            timestamp: ts,
                            payload_size: bytes.len() as u32,
                        });
                    }
                }

                let next_send = start + period.mul_f64(local_seq as f64);
                let now = std::time::Instant::now();
                if next_send > now {
                    thread::sleep(next_send - now);
                }
            }

            // Give a moment for late echoes.
            let deadline = std::time::Instant::now() + Duration::from_millis(500);
            while std::time::Instant::now() < deadline {
                if let Ok(n) = socket.recv(&mut recv_buf) {
                    if let Ok(echoed) = ProbePacket::decode(&recv_buf[..n]) {
                        let recv_ts = Timestamp::now();
                        {
                            let mut s = stats.lock().unwrap();
                            s.packets_received += 1;
                            s.bytes_received += n as u64;
                            s.last_seq_recv = echoed.sequence_number;
                        }
                        if let Ok(mut w) = writer.lock() {
                            if let Some(ref mut wr) = *w {
                                let _ = wr.append(&PacketRow {
                                    flow_id,
                                    sequence_number: echoed.sequence_number,
                                    direction: Direction::Received,
                                    timestamp: recv_ts,
                                    payload_size: n as u32,
                                });
                            }
                        }
                    }
                } else {
                    thread::sleep(Duration::from_millis(10));
                }
            }

            // Flush and drop writer
            if let Ok(mut w) = writer.lock() {
                if let Some(mut wr) = w.take() {
                    let _ = wr.flush();
                    drop(wr);
                }
            }
        });

        *self.flow_handle.lock().unwrap() = Some(handle);
        Ok(())
    }

    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(h) = self.flow_handle.lock().unwrap().take() {
            let _ = h.join();
        }
    }

    pub fn poll_stats(&self) -> FlowStats {
        self.stats.lock().unwrap().clone()
    }
}

impl Drop for CoreState {
    fn drop(&mut self) {
        self.stop();
        if let Ok(mut w) = self.writer.lock() {
            drop(w.take());
        }
    }
}

// ------------------------------------------------------------------
// JNI exports
// ------------------------------------------------------------------

fn jstring_to_rust(env: &mut JNIEnv, js: &JString) -> Option<String> {
    if js.is_null() {
        return None;
    }
    let s: JavaStr = env.get_string(js).ok()?;
    Some(s.to_str().ok()?.to_owned())
}

fn rust_to_jstring<'a>(env: &mut JNIEnv<'a>, s: &str) -> jni::objects::JString<'a> {
    env.new_string(s).expect("JNI new_string failed")
}

#[no_mangle]
pub extern "system" fn Java_spike_RustBridge_init(
    mut env: JNIEnv,
    _class: jni::objects::JClass,
    config_json: JString,
) -> jlong {
    let cfg_str = match jstring_to_rust(&mut env, &config_json) {
        Some(s) => s,
        None => {
            let _ = env.throw_new("java/lang/IllegalArgumentException", "null config");
            return 0;
        }
    };
    let config: CoreConfig = match serde_json::from_str(&cfg_str) {
        Ok(c) => c,
        Err(e) => {
            let _ = env.throw_new(
                "java/lang/IllegalArgumentException",
                format!("bad config: {}", e),
            );
            return 0;
        }
    };

    let state = match CoreState::new(config) {
        Ok(s) => s,
        Err(e) => {
            let _ = env.throw_new(
                "java/lang/RuntimeException",
                format!("init failed: {}", e),
            );
            return 0;
        }
    };

    let boxed = Box::new(state);
    Box::into_raw(boxed) as jlong
}

#[no_mangle]
pub extern "system" fn Java_spike_RustBridge_startFlow(
    mut env: JNIEnv,
    _class: jni::objects::JClass,
    handle: jlong,
    flow_json: JString,
) {
    if handle == 0 {
        let _ = env.throw_new("java/lang/IllegalArgumentException", "null handle");
        return;
    }
    let state = unsafe { &*(handle as *const CoreState) };
    let flow_str = match jstring_to_rust(&mut env, &flow_json) {
        Some(s) => s,
        None => {
            let _ = env.throw_new("java/lang/IllegalArgumentException", "null flow config");
            return;
        }
    };
    let flow: FlowConfig = match serde_json::from_str(&flow_str) {
        Ok(f) => f,
        Err(e) => {
            let _ = env.throw_new(
                "java/lang/IllegalArgumentException",
                format!("bad flow config: {}", e),
            );
            return;
        }
    };

    if let Err(e) = state.start_flow(flow) {
        let _ = env.throw_new("java/lang/RuntimeException", format!("startFlow failed: {}", e));
    }
}

#[no_mangle]
pub extern "system" fn Java_spike_RustBridge_pollStatus<'a>(
    mut env: JNIEnv<'a>,
    _class: jni::objects::JClass<'a>,
    handle: jlong,
) -> jni::objects::JString<'a> {
    if handle == 0 {
        let _ = env.throw_new("java/lang/IllegalArgumentException", "null handle");
        return rust_to_jstring(&mut env, "{}");
    }
    let state = unsafe { &*(handle as *const CoreState) };
    let stats = state.poll_stats();
    let json = serde_json::to_string(&stats).unwrap_or_else(|_| "{}".to_string());
    rust_to_jstring(&mut env, &json)
}

#[no_mangle]
pub extern "system" fn Java_spike_RustBridge_stop(
    mut env: JNIEnv,
    _class: jni::objects::JClass,
    handle: jlong,
) {
    if handle == 0 {
        let _ = env.throw_new("java/lang/IllegalArgumentException", "null handle");
        return;
    }
    let state = unsafe { &*(handle as *const CoreState) };
    state.stop();
}

#[no_mangle]
pub extern "system" fn Java_spike_RustBridge_destroy(
    mut env: JNIEnv,
    _class: jni::objects::JClass,
    handle: jlong,
) {
    if handle == 0 {
        let _ = env.throw_new("java/lang/IllegalArgumentException", "null handle");
        return;
    }
    let _ = unsafe { Box::from_raw(handle as *mut CoreState) };
}

#[no_mangle]
pub extern "system" fn Java_spike_RustBridge_sampleClock<'a>(
    mut env: JNIEnv<'a>,
    _class: jni::objects::JClass<'a>,
) -> jni::objects::JString<'a> {
    let ts = Timestamp::now();
    let json = serde_json::to_string(&ts).unwrap_or_else(|_| "{}".to_string());
    rust_to_jstring(&mut env, &json)
}
