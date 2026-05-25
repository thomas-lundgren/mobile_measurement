//! Logging-first dependency direction (invariant 1, dev-plan §6): the shared
//! measurement/logging core must not be able to reach an upload/HTTP client, so
//! the upload path can never sit on the measurement hot path. Enforced
//! structurally by asserting the core crate declares no HTTP-client dependency.

const FORBIDDEN: &[&str] = &["reqwest", "hyper", "ureq", "isahc", "curl", "surf"];

#[test]
fn core_has_no_upload_client_dependency() {
    let manifest = include_str!("../Cargo.toml");
    // crude but sufficient: scan declared dependency names.
    for dep in FORBIDDEN {
        assert!(
            !manifest.contains(dep),
            "core must not depend on an HTTP/upload client ({dep}); \
             the measurement path cannot reach the upload client (invariant 1)"
        );
    }
}
