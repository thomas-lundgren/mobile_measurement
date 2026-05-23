//! Parity tests — the drift gate. These read the YAML/JSON under `contracts/`
//! and assert the Rust enums / consts / DTOs match. If a contract changes
//! without the Rust side following, these fail.

use std::collections::BTreeSet;
use std::path::PathBuf;

use mp_contracts::*;

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
