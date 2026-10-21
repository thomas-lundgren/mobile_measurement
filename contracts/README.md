# `contracts/` — single source of truth

Every cross-component boundary lives here, versioned in one repo. If a value
appears in two languages on the wire, it is generated from this directory.
Rationale and full strategy: `planning/development_guide.md` §4 and §8;
`planning/development_plan.md` §2.

## Layout

| Path | Owns | Authored as | Generated for |
|---|---|---|---|
| `versions/versions.yaml` | `protocol_version`, `schema_version`, `auth_scheme_version`, AUTH_* constants | YAML (hand-authored) | Rust consts, Java consts, Python module |
| `enums/enums.yaml` | Command names, state-machine names, flow types, direction, role, platform, event types, test results | YAML (hand-authored) | Rust enums, Java enums, Python Literals |
| `openapi/` | REST API (device-facing + controller-facing) | **Code-first via `utoipa`** on the Rust server — emitted as `openapi/openapi.yaml` | Rust types (native), Kotlin client (Android Controller) |
| `internal/` | Server-side control IPC (server-main → test-control → data-generator) + the in-process generator→logger sink | Markdown + JSON Schema (`internal_*`) over **local HTTP+JSON** | server processes (axum/serde); not public |
| `packets/` | Wire-format layout tables for `ProbePacket`, `RealtimePacket` | Markdown layout table + golden hex vectors | Encode/decode conformance tests in every language |
| `schemas/` | `header.json`, `summary.json`, capability, logging-config, test_plan, RUN_TEST, upload manifest/init | JSON Schema | DTO derivation; tolerant readers in analysis |
| `upload/` | Upload protocol (phases, resume, compression, test-result reporting) | Markdown spec | Upload manager (mobile) + upload handler (server) |
| `parquet/` | Arrow schema per log file | Arrow schema (`.fbs` or YAML) | Rust writer (`core/log_writer.rs`), Python loader |
| `auth/` | (reserved) | — | AUTH_* constants currently colocated in `versions/versions.yaml` |
| `testvectors/` | Cross-language golden inputs/outputs | Hex / JSON / DER bytes + expected outputs | Conformance tests in every language |

## Drift gates (CI)

- **`openapi/openapi.yaml`** regenerated from server source on each build;
  diff against committed file → fail if non-empty.
- **`enums/enums.yaml`** parsed by each language's codegen step; the
  generated artefact is committed and diffed.
- **`testvectors/`** is mandatory: every codec (packets, auth signatures)
  must round-trip the vectors in CI. New vectors are added with new
  features; old vectors are never deleted (forward-compat invariant).
- **Schemas are additive-only**: a CI rule rejects PRs that remove or
  rename a column / field without bumping `schema_version`.
- **JSON Schema vs Rust types**: `ci/validate_contracts.py` compares every
  schema that has a Rust DTO (same title in the generated OpenAPI): property
  names, required fields, enum values and flow variants.
- **Parquet**: `core/tests/parquet_contract.rs` writes every table and
  compares its schema (names, order, types, nullability) with
  `parquet/*.yaml`; `analysis/tests/test_schema_contract.py` does the same
  for the analysis column lists.
- **App activities**: `exchange::activities_match_the_contract` compares the
  wire table with `enums.yaml` `app_activities`.

**Not generated or checked yet** (see CLAUDE.md invariant 7): the Java
Logger writes command/state strings, version constants and `header.json` by
hand (no Java codegen exists despite the table above); the mp-probe protocol
and the reach protocol have no vectors.

## Authoring rules

1. **Never hand-edit a generated file.** If the OpenAPI YAML, Rust
   constants, or Java enums look wrong, edit the source (server DTOs or
   `enums.yaml` / `versions.yaml`) and regenerate.
2. **Wire-format strings come from `enums.yaml`** — do not inline
   `"RUN_TEST"` or `"FromServer"` in code. Import the generated constant.
3. **Version bumps are deliberate.** Additive changes to schemas do NOT
   bump `schema_version`; non-additive changes (remove, rename, change
   type) do. Bump policy in `versions/versions.yaml`.
4. **OpenAPI is code-first.** `openapi/openapi.yaml` is generated from the
   `server-main` handlers (`#[utoipa::path]` + the `ApiDoc` in
   `server/server-main/src/openapi.rs`) and committed. Never hand-edit it;
   regenerate with `MP_REGEN_OPENAPI=1 cargo test -p mp-server-main --test
   openapi_drift`. A CI drift gate asserts it is current. (The earlier
   `planning/*_openapi_sketch.yaml` design sketches have been deleted.)

## Status

| Contract | State |
|---|---|
| `versions/versions.yaml` | drafted |
| `enums/enums.yaml` | drafted |
| `packets/` | drafted — Probe v1 (`0x01`) and Realtime v1 (`0x02`) with `(pkt_t, ver)` framing; growth by `ver` bump, no runtime extensibility. `core/src/packet.rs` (Stage-A spike code, unframed 20-byte probe) needs Phase-1 update to match. |
| `testvectors/packets/` | drafted — `probe_v1.json`, `realtime_v1.json`; round-trip verified |
| `schemas/test_plan.schema.json` + `schemas/run_test_command.schema.json` | drafted — TestPlan is **port-less** (Flow $defs are the single source of flow shape); server-allocated ports live in `flow_bindings: [{flow_id, server_port}]` on RunTestPayload + header, keyed by flow_id. RunTestPayload = test_id + server_host + test_plan + flow_bindings. Examples validated. |
| `schemas/capability.schema.json` + `schemas/logging_config.schema.json` | drafted — radio / qos / mobility feature groups + timestamp_sources (capability) and + events / gps / clock_calibration_interval_sec (config). `supported_features` dropped (remediation 1.4). Examples validated. |
| `upload/README.md` + `schemas/upload_manifest.schema.json` + `schemas/upload_init_response.schema.json` | drafted — 3-phase resumable per-file upload; test_result + PARTIAL definition (remediation 1.3 + M4b). Examples validated. |
| `parquet/*.yaml` (packets / radio / gps / clock / events) | drafted — Arrow schemas with dual clock, join columns, nullable capability fields; `core/log_writer.rs` packets schema needs extending to match (currently 6 cols, contract has more). Examples not binary-tested yet. |
| `schemas/header.schema.json` + `schemas/summary.schema.json` | drafted — header = versions + identities + test_plan + logging_config ($ref); summary = per-flow/radio/gps/upload stats (all optional). Examples validated. |
| `schemas/device_status.schema.json` | drafted — POST /status heartbeat: logger_state, current_test_id, logging_config_version, battery, storage, pending_uploads, radio snapshot (remediation 3.6). Examples validated. |
| `schemas/create_test_request.schema.json` | drafted — operator create-test body; `CreateTestRequest.plan` is exactly a `$ref` to TestPlan (no duplicated flow defs) (remediation 3.7). The controller REST surface is now in the generated `openapi/openapi.yaml`. Examples validated. |
| `internal/` (control_api + logger_sink) + `schemas/internal_*.json` | drafted — server-side control IPC over local HTTP+JSON + the generator→logger ring contract (remediation 3.4). Examples validated. |
| `db/schema.sql` + `db/README.md` | drafted — server-main SQLite schema, 12 tables (auth / registry / orchestration / upload), enum CHECKs from enums.yaml (remediation 3.3). Executes + constraint-tested. |
| `openapi/openapi.yaml` | **generated** — code-first via `utoipa` from `server-main` handlers (`#[utoipa::path]` + `ApiDoc`). Regenerate: `MP_REGEN_OPENAPI=1 cargo test -p mp-server-main --test openapi_drift`; CI drift gate asserts it's current. Replaced the `planning/*_openapi_sketch.yaml` sketches (deleted). |
| `testvectors/auth_signing/vector.json` | drafted — cross-platform ECDSA P-256 signing vector (fixed key, deterministic sig); round-tripped by `core::auth` (sign reproduces it + verify). Generated by `core/examples/gen_auth_vector.rs`. |
