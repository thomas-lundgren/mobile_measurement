//! Drift gate: the committed `contracts/openapi/openapi.yaml` must equal what
//! the code generates. Regenerate with:
//!   MP_REGEN_OPENAPI=1 cargo test -p mp-server-main --test openapi_drift

use std::path::PathBuf;

fn committed_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../contracts/openapi/openapi.yaml")
}

fn generated() -> String {
    serde_yaml::to_string(&mp_server_main::openapi::document()).unwrap()
}

#[test]
fn openapi_is_current() {
    let generated = generated();
    let path = committed_path();

    if std::env::var("MP_REGEN_OPENAPI").is_ok() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &generated).unwrap();
        eprintln!("regenerated {}", path.display());
        return;
    }

    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        generated, committed,
        "OpenAPI drift — regenerate: MP_REGEN_OPENAPI=1 cargo test -p mp-server-main --test openapi_drift"
    );
}
