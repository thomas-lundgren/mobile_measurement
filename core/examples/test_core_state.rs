use fieldtest_core::{CoreState, CoreConfig, FlowConfig};
use std::path::PathBuf;

fn main() {
    let dir = PathBuf::from("/tmp/test_core_state_logs");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    
    let config = CoreConfig {
        node_id: "test".to_string(),
        log_dir: dir.to_string_lossy().to_string(),
        segment_rows: 100,
    };
    
    let state = CoreState::new(config).unwrap();
    let flow = FlowConfig {
        flow_id: 1,
        server_host: "127.0.0.1".to_string(),
        server_port: 41000,
        local_port: None,
        rate_pps: 10,
        packet_size: 64,
        duration_sec: 2,
    };
    
    state.start_flow(flow).unwrap();
    std::thread::sleep(std::time::Duration::from_secs(3));
    state.stop();
    
    // Simulate destroy
    drop(state);
    
    let files: Vec<_> = std::fs::read_dir(&dir).unwrap()
        .filter_map(|e| e.ok())
        .map(|e| (e.path(), e.metadata().unwrap().len()))
        .collect();
    for (p, s) in &files {
        println!("{:?}: {} bytes", p, s);
    }
}
