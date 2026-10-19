//! Parity tests — the drift gate. These read the YAML/JSON under `contracts/`
//! and assert the Rust enums / consts / DTOs match. If a contract changes
//! without the Rust side following, these fail.

use std::collections::BTreeSet;
use std::path::PathBuf;

use mp_contracts::{enums::*, versions::*, *};

fn contracts_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR = server/contracts-rs
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../contracts")
}

fn yaml(path: &str) -> serde_yaml::Value {
    let p = contracts_dir().join(path);
    let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {p:?}: {e}"));
    serde_yaml::from_str(&text).unwrap_or_else(|e| panic!("parse {p:?}: {e}"))
}

fn json(path: &str) -> serde_json::Value {
    let p = contracts_dir().join(path);
    let text = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {p:?}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {p:?}: {e}"))
}

/// Pull a top-level string sequence out of enums.yaml.
fn yaml_str_set(v: &serde_yaml::Value, key: &str) -> BTreeSet<String> {
    v.get(key)
        .unwrap_or_else(|| panic!("enums.yaml missing key {key}"))
        .as_sequence()
        .unwrap_or_else(|| panic!("enums.yaml {key} is not a sequence"))
        .iter()
        .map(|x| x.as_str().expect("enum entry not a string").to_string())
        .collect()
}

fn rust_set<T: Copy>(all: &[T], wire: impl Fn(T) -> &'static str) -> BTreeSet<String> {
    all.iter().map(|&v| wire(v).to_string()).collect()
}

#[test]
fn enums_match_yaml() {
    let e = yaml("enums/enums.yaml");

    let cases: Vec<(&str, BTreeSet<String>)> = vec![
        ("commands", rust_set(Command::ALL, Command::wire)),
        (
            "test_control_states",
            rust_set(TestControlState::ALL, TestControlState::wire),
        ),
        (
            "generator_states",
            rust_set(GeneratorState::ALL, GeneratorState::wire),
        ),
        (
            "logger_states",
            rust_set(LoggerState::ALL, LoggerState::wire),
        ),
        ("flow_types", rust_set(FlowType::ALL, FlowType::wire)),
        ("directions", rust_set(Direction::ALL, Direction::wire)),
        ("roles", rust_set(Role::ALL, Role::wire)),
        (
            "operator_roles",
            rust_set(OperatorRole::ALL, OperatorRole::wire),
        ),
        ("platforms", rust_set(Platform::ALL, Platform::wire)),
        ("event_types", rust_set(EventType::ALL, EventType::wire)),
        ("test_results", rust_set(TestResult::ALL, TestResult::wire)),
        (
            "device_roles",
            rust_set(DeviceRole::ALL, DeviceRole::wire),
        ),
        ("vpn_scopes", rust_set(VpnScope::ALL, VpnScope::wire)),
        ("probe_kinds", rust_set(ProbeKind::ALL, ProbeKind::wire)),
    ];

    for (key, rust) in cases {
        let y = yaml_str_set(&e, key);
        assert_eq!(rust, y, "enum drift for `{key}`: rust={rust:?} yaml={y:?}");
    }
}

#[test]
fn versions_match_yaml() {
    let v = yaml("versions/versions.yaml");
    let ver = v.get("versions").expect("versions key");
    assert_eq!(
        ver.get("protocol_version").unwrap().as_u64().unwrap() as u32,
        PROTOCOL_VERSION
    );
    assert_eq!(
        ver.get("schema_version").unwrap().as_u64().unwrap() as u32,
        SCHEMA_VERSION
    );
    assert_eq!(
        ver.get("auth_scheme_version").unwrap().as_u64().unwrap() as u32,
        AUTH_SCHEME_VERSION
    );
    assert_eq!(
        ver.get("logging_config_version_initial")
            .unwrap()
            .as_u64()
            .unwrap() as u32,
        LOGGING_CONFIG_VERSION_INITIAL
    );

    let auth = v.get("auth_constants").expect("auth_constants key");
    assert_eq!(
        auth.get("AUTH_SIGNING_ALG").unwrap().as_str().unwrap(),
        AUTH_SIGNING_ALG
    );
    assert_eq!(
        auth.get("AUTH_PUBKEY_FORMAT").unwrap().as_str().unwrap(),
        AUTH_PUBKEY_FORMAT
    );
    assert_eq!(
        auth.get("AUTH_SIG_FORMAT").unwrap().as_str().unwrap(),
        AUTH_SIG_FORMAT
    );
    assert_eq!(
        auth.get("AUTH_DOMAIN").unwrap().as_str().unwrap(),
        AUTH_DOMAIN
    );

    let app = v.get("app_constants").expect("app_constants key");
    assert_eq!(
        app.get("MP_LOGGER_PACKAGE").unwrap().as_str().unwrap(),
        MP_LOGGER_PACKAGE
    );
    assert_eq!(
        app.get("MP_MONITOR_PACKAGE").unwrap().as_str().unwrap(),
        MP_MONITOR_PACKAGE
    );
}

#[test]
fn monitor_logging_config_round_trip() {
    use mp_contracts::{CaptureConfig, PingConfig, PingTarget};
    use mp_contracts::enums::VpnScope;

    let cfg = mp_contracts::LoggingConfig {
        version: 1,
        radio: mp_contracts::RadioLogging {
            serving_cell: vec![],
            neighbor_cells: false,
            lte_fallback: false,
            cqi: false, mcs: false, bler: false, harq: false,
            drx: false, bwp: false, mimo_layers: false, tx_power: false,
        },
        qos: mp_contracts::QosLogging { qi_5: false, qfi: false, slice_info: false },
        mobility: mp_contracts::MobilityLogging {
            explicit_ho_events: false, rrc_state: false, beam_switch: false,
        },
        events: vec![],
        gps: false,
        clock_calibration_interval_sec: 30,
        capture: Some(CaptureConfig {
            enabled: true,
            scope: VpnScope::Allowlist,
            apps: vec!["com.spotify.music".to_string()],
            quota_mb_per_day: Some(500),
        }),
        ping: Some(PingConfig {
            enabled: true,
            default_interval_ms: 1000,
            targets: vec![PingTarget {
                label: "cloudflare".to_string(),
                host: "1.1.1.1".to_string(),
                interval_ms: None,
            }],
            follow_apps: vec!["com.spotify.music".to_string()],
            auto_target_count: 5,
        }),
    };
    let round: mp_contracts::LoggingConfig =
        serde_json::from_value(serde_json::to_value(&cfg).unwrap()).unwrap();
    assert_eq!(cfg, round);
}

#[test]
fn monitor_device_status_round_trip() {
    use mp_contracts::{LinkState, InterfaceLinkState, MonitorState};
    use mp_contracts::enums::VpnScope;

    let status = mp_contracts::DeviceStatus {
        logger_state: mp_contracts::enums::LoggerState::Idle,
        current_test_id: None,
        logging_config_version: 1,
        sampled_at: mp_contracts::DualClock { mono_ns: 1_000_000, utc_ns: 1_700_000_000_000_000_000 },
        battery: None,
        storage: None,
        pending_uploads: None,
        radio: None,
        link_state: Some(LinkState {
            rmnet: Some(InterfaceLinkState {
                default_gw: "100.72.0.1".to_string(),
                since_mono_ns: Some(500_000_000),
            }),
            wlan: None,
        }),
        monitor_state: Some(MonitorState {
            active: true,
            scope: Some(VpnScope::All),
            apps_inventory_version: Some(3),
            bytes_observed: 1_234_567,
            bytes_today: 500_000,
            quota_remaining_mb: Some(450),
            buckets_pending: 2,
            pings_sent: 120,
            pings_lost: 1,
            top_peers: vec![],
        }),
    };
    let round: mp_contracts::DeviceStatus =
        serde_json::from_value(serde_json::to_value(&status).unwrap()).unwrap();
    assert_eq!(status, round);
}

#[test]
fn capability_supports_ping_round_trip() {
    // Logger capability with supports_ping
    let logger_cap = mp_contracts::Capability {
        radio: mp_contracts::RadioCapability {
            serving_cell: vec![],
            neighbor_cells: false, lte_fallback: false, cqi: false, mcs: false,
            bler: false, harq: false, drx: false, bwp: false, mimo_layers: false,
            tx_power: false,
        },
        qos: mp_contracts::QosCapability { qi_5: false, qfi: false, slice_info: false },
        mobility: mp_contracts::MobilityCapability {
            explicit_ho_events: false, rrc_state: false, beam_switch: false,
        },
        timestamp_sources: vec![mp_contracts::TimestampSource::Mono, mp_contracts::TimestampSource::Utc],
        supports_ping: true,
        monitor: None,
    };
    let round: mp_contracts::Capability =
        serde_json::from_value(serde_json::to_value(&logger_cap).unwrap()).unwrap();
    assert_eq!(logger_cap, round);

    // Old Logger enrollment without supports_ping still deserializes (default false)
    let old_cap = serde_json::json!({
        "radio": { "serving_cell": ["rsrp"], "neighbor_cells": false, "lte_fallback": false,
                   "cqi": false, "mcs": false, "bler": false, "harq": false,
                   "drx": false, "bwp": false, "mimo_layers": false, "tx_power": false },
        "qos": { "5qi": false, "qfi": false, "slice_info": false },
        "mobility": { "explicit_ho_events": false, "rrc_state": false, "beam_switch": false },
        "timestamp_sources": ["mono", "utc"]
    });
    let parsed: mp_contracts::Capability = serde_json::from_value(old_cap).unwrap();
    assert!(!parsed.supports_ping);
    assert!(parsed.monitor.is_none());
}

#[test]
fn app_list_round_trip() {
    let list = mp_contracts::AppList {
        device_id: "dev_01KSHHPW83SJPQCVR6HVZH0SAQ".to_string(),
        inventory_version: 5,
        collected_utc_ns: 1_700_000_000_000_000_000,
        apps: vec![
            mp_contracts::AppInfo {
                package_name: "com.spotify.music".to_string(),
                app_label: "Spotify".to_string(),
                uid: 10_123,
                version_name: Some("8.9.2".to_string()),
                is_system: false,
                installed_utc_ns: Some(1_699_000_000_000_000_000),
            },
            mp_contracts::AppInfo {
                package_name: "com.android.chrome".to_string(),
                app_label: "Chrome".to_string(),
                uid: 10_034,
                version_name: None,
                is_system: false,
                installed_utc_ns: None,
            },
        ],
    };
    let round: mp_contracts::AppList =
        serde_json::from_value(serde_json::to_value(&list).unwrap()).unwrap();
    assert_eq!(list, round);
}

#[test]
fn enroll_request_role_round_trip() {
    use mp_contracts::enums::DeviceRole;

    let req = mp_contracts::EnrollRequest {
        device_pubkey: "base64pubkey==".to_string(),
        pubkey_format: "SPKI_DER".to_string(),
        sig_alg: "ECDSA_P256_SHA256".to_string(),
        sig_format: "ASN1_DER".to_string(),
        platform: mp_contracts::enums::Platform::Android,
        hw_info: Default::default(),
        protocol_version: 1,
        role: Some(DeviceRole::Monitor),
        hardware_id: Some("a3f8bc1200000000".to_string()),
    };
    let round: mp_contracts::EnrollRequest =
        serde_json::from_value(serde_json::to_value(&req).unwrap()).unwrap();
    assert_eq!(req, round);

    // Old Logger enrollment without role/hardware_id must still deserialize.
    let old = serde_json::json!({
        "device_pubkey": "base64pubkey==",
        "pubkey_format": "SPKI_DER",
        "sig_alg": "ECDSA_P256_SHA256",
        "sig_format": "ASN1_DER",
        "platform": "android",
        "protocol_version": 1
    });
    let parsed: mp_contracts::EnrollRequest = serde_json::from_value(old).unwrap();
    assert_eq!(parsed.role, None);
    assert_eq!(parsed.hardware_id, None);
}

#[test]
fn capability_examples_deserialize() {
    let doc = json("testvectors/capability_examples.json");
    let examples = doc["examples"].as_array().unwrap();
    assert!(!examples.is_empty());
    for ex in examples {
        let name = ex["name"].as_str().unwrap();
        let cap: Capability = serde_json::from_value(ex["capability"].clone())
            .unwrap_or_else(|e| panic!("capability example `{name}`: {e}"));
        // round-trip back to JSON and re-parse
        let round: Capability =
            serde_json::from_value(serde_json::to_value(&cap).unwrap()).unwrap();
        assert_eq!(cap, round, "capability round-trip `{name}`");
    }
}

#[test]
fn logging_config_examples_deserialize() {
    let doc = json("testvectors/logging_config_examples.json");
    for ex in doc["examples"].as_array().unwrap() {
        let name = ex["name"].as_str().unwrap();
        let cfg: LoggingConfig = serde_json::from_value(ex["config"].clone())
            .unwrap_or_else(|e| panic!("logging_config example `{name}`: {e}"));
        let round: LoggingConfig =
            serde_json::from_value(serde_json::to_value(&cfg).unwrap()).unwrap();
        assert_eq!(cfg, round, "logging_config round-trip `{name}`");
    }
}

#[test]
fn test_plan_examples_deserialize() {
    // test_plan is nested inside the RUN_TEST payload examples.
    let doc = json("testvectors/commands/run_test_examples.json");
    for ex in doc["examples"].as_array().unwrap() {
        let name = ex["name"].as_str().unwrap();
        let plan: TestPlan = serde_json::from_value(ex["payload"]["test_plan"].clone())
            .unwrap_or_else(|e| panic!("test_plan in run_test example `{name}`: {e}"));
        assert!(!plan.flows.is_empty());
        let round: TestPlan = serde_json::from_value(serde_json::to_value(&plan).unwrap()).unwrap();
        assert_eq!(plan, round, "test_plan round-trip `{name}`");

        // flow_bindings parse too
        let bindings: Vec<FlowBinding> =
            serde_json::from_value(ex["payload"]["flow_bindings"].clone()).unwrap();
        assert_eq!(bindings.len(), plan.flows.len(), "binding count `{name}`");
    }
}

#[test]
fn reachability_examples_deserialize() {
    let doc = json("testvectors/reachability/reachability_report_examples.json");
    let examples = doc["examples"].as_array().unwrap();
    assert!(!examples.is_empty());
    for ex in examples {
        let name = ex["name"].as_str().unwrap();
        let report: ReachabilityReport = serde_json::from_value(ex["report"].clone())
            .unwrap_or_else(|e| panic!("reachability example `{name}`: {e}"));
        let round: ReachabilityReport =
            serde_json::from_value(serde_json::to_value(&report).unwrap()).unwrap();
        assert_eq!(report, round, "reachability round-trip `{name}`");
    }
}
