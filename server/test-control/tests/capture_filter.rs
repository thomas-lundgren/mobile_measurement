//! The capture's BPF filter takes the ports of the test's TCP flows only.

use mp_contracts::Flow;
use mp_test_control::capture::filter;

#[test]
fn filter_takes_the_tcp_flows_ports() {
    let flows: Vec<Flow> = serde_json::from_value(serde_json::json!([
        { "flow_id": 1, "type": "probe", "rate_pps": 10 },
        { "flow_id": 2, "type": "bulk_tcp", "direction": "FromServer" },
        { "flow_id": 3, "type": "app_traffic", "profile": "web" },
    ]))
    .unwrap();
    let bound: Vec<(&Flow, u16)> = flows.iter().zip([41000, 41001, 41002]).collect();
    assert_eq!(
        filter(&bound).as_deref(),
        Some("tcp port 41001 or port 41002")
    );
    assert_eq!(filter(&bound[..1]), None);
}
