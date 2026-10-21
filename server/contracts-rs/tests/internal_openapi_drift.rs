//! Drift gate: the committed `contracts/openapi/internal.yaml` must equal what
//! the internal DTOs generate. Regenerate with:
//!   MP_REGEN_OPENAPI=1 cargo test -p mp-contracts --test internal_openapi_drift

use std::path::PathBuf;
use utoipa::OpenApi;

#[test]
fn internal_openapi_is_current() {
    let generated = serde_yaml::to_string(&mp_contracts::InternalApiDoc::openapi()).unwrap();
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../contracts/openapi/internal.yaml");

    if std::env::var("MP_REGEN_OPENAPI").is_ok() {
        std::fs::write(&path, &generated).unwrap();
        eprintln!("regenerated {}", path.display());
        return;
    }

    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        generated, committed,
        "internal OpenAPI drift — regenerate: MP_REGEN_OPENAPI=1 cargo test -p mp-contracts --test internal_openapi_drift"
    );
}
