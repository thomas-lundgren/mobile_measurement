//! `mp-contracts` — the Rust face of `contracts/`.
//!
//! Enums and version/auth constants mirror `contracts/enums/enums.yaml` and
//! `contracts/versions/versions.yaml` and are guarded by parity tests
//! (`tests/parity.rs`) that read those YAML files — the parity test is the
//! drift gate (see `planning/phase1_detailed_design.md` §5). DTOs mirror the
//! JSON Schemas and derive `utoipa::ToSchema` so Phase-1's `server-main` can
//! emit the authoritative OpenAPI.

pub mod dto;
pub mod enums;
pub mod versions;

pub use dto::*;
pub use enums::*;
pub use versions::*;
