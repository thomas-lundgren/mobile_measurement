//! The UE engine reads every field a test plan can carry. The Logger hands
//! each RUN_TEST flow to the core whole (plus its server port), so a flow
//! field the core does not know would be silently ignored on the phone. This
//! checks the contract's examples field by field, and that the examples use
//! every flow property in `contracts/schemas/test_plan.schema.json`.

use std::collections::BTreeSet;
use std::path::PathBuf;

use fieldtest_core::ue::UeTestConfig;
use serde_json::{json, Value};

/// Flow types the UE engine does not run yet (their fields have no reader).
const NOT_ON_THE_UE: [&str; 1] = ["gateway_ping"];

fn contracts() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../contracts")
}

fn read(rel: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(contracts().join(rel)).unwrap()).unwrap()
}

/// What the Logger builds from a RUN_TEST payload (LoggerService.buildUeConfig).
fn ue_config(payload: &Value) -> Value {
    let flows: Vec<Value> = payload["test_plan"]["flows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            let mut f = f.clone();
            let port = payload["flow_bindings"]
                .as_array()
                .unwrap()
                .iter()
                .find(|b| b["flow_id"] == f["flow_id"])
                .map(|b| b["server_port"].clone())
                .unwrap_or(Value::Null);
            f["server_port"] = port;
            f
        })
        .collect();
    json!({
        "test_id": payload["test_id"],
        "device_id": "dev_1",
        "session_id": payload["session_id"],
        "server_host": payload["server_host"],
        "log_dir": "/tmp/mobile",
        "flows": flows,
    })
}

#[test]
fn the_core_reads_every_field_of_the_example_flows() {
    let doc = read("testvectors/commands/run_test_examples.json");
    for ex in doc["examples"].as_array().unwrap() {
        let name = ex["name"].as_str().unwrap();
        let mut payload = ex["payload"].clone();
        // stub flow types are not judged
        if let Some(flows) = payload["test_plan"]["flows"].as_array_mut() {
            flows.retain(|f| !NOT_ON_THE_UE.contains(&f["type"].as_str().unwrap()));
        }
        let cfg = ue_config(&payload);
        let mut ignored = Vec::new();
        let _: UeTestConfig =
            serde_ignored::deserialize(&cfg, |path| ignored.push(path.to_string()))
                .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(ignored.is_empty(), "{name}: the core ignores {ignored:?}");
    }
}

#[test]
fn the_examples_use_every_flow_property() {
    let schema = read("schemas/test_plan.schema.json");
    let defs = schema["$defs"].as_object().unwrap();
    let mut used: BTreeSet<(String, String)> = BTreeSet::new();
    for ex in read("testvectors/commands/run_test_examples.json")["examples"]
        .as_array()
        .unwrap()
    {
        for f in ex["payload"]["test_plan"]["flows"].as_array().unwrap() {
            let t = f["type"].as_str().unwrap().to_string();
            for k in f.as_object().unwrap().keys() {
                used.insert((t.clone(), k.clone()));
            }
        }
    }
    let mut missing = Vec::new();
    for def in defs.values() {
        let Some(t) = def["properties"]["type"]["const"].as_str() else {
            continue;
        };
        if NOT_ON_THE_UE.contains(&t) {
            continue;
        }
        for k in def["properties"].as_object().unwrap().keys() {
            if !used.contains(&(t.to_string(), k.clone())) {
                missing.push(format!("{t}.{k}"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "no run_test example uses {missing:?}: add one to contracts/testvectors/commands/run_test_examples.json"
    );
}
