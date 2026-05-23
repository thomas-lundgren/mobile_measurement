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

## Authoring rules

1. **Never hand-edit a generated file.** If the OpenAPI YAML, Rust
   constants, or Java enums look wrong, edit the source (server DTOs or
   `enums.yaml` / `versions.yaml`) and regenerate.
2. **Wire-format strings come from `enums.yaml`** — do not inline
   `"RUN_TEST"` or `"FromServer"` in code. Import the generated constant.
3. **Version bumps are deliberate.** Additive changes to schemas do NOT
   bump `schema_version`; non-additive changes (remove, rename, change
   type) do. Bump policy in `versions/versions.yaml`.
4. **OpenAPI is code-first.** During the planning phase the file
   `planning/auth_openapi_sketch.yaml` is a design artefact, not the
   source of truth; it will be deleted when the utoipa-generated
   `openapi/openapi.yaml` lands in Phase 1.

## Status

| Contract | State |
|---|---|
| `versions/versions.yaml` | drafted |
| `enums/enums.yaml` | drafted |
| `packets/` | drafted — Probe v1 (`0x01`) and Realtime v1 (`0x02`) with `(pkt_t, ver)` framing; growth by `ver` bump, no runtime extensibility. `core/src/packet.rs` (Stage-A spike code, unframed 20-byte probe) needs Phase-1 update to match. |
| `testvectors/packets/` | drafted — `probe_v1.json`, `realtime_v1.json`; round-trip verified |
| `schemas/test_plan.schema.json` + `schemas/run_test_command.schema.json` | drafted — TestPlan, Flow ($defs: ProbeFlow / RealtimeUdpFlow / BulkTcpFlow), RunTestPayload. Examples validated. |
| `schemas/capability.schema.json` + `schemas/logging_config.schema.json` | drafted — radio / qos / mobility feature groups + timestamp_sources (capability) and + events / gps / clock_calibration_interval_sec (config). `supported_features` dropped (remediation 1.4). Examples validated. |
| `upload/README.md` + `schemas/upload_manifest.schema.json` + `schemas/upload_init_response.schema.json` | drafted — 3-phase resumable per-file upload; test_result + PARTIAL definition (remediation 1.3 + M4b). Examples validated. |
| `parquet/*.yaml` (packets / radio / gps / clock / events) | drafted — Arrow schemas with dual clock, join columns, nullable capability fields; `core/log_writer.rs` packets schema needs extending to match (currently 6 cols, contract has more). Examples not binary-tested yet. |
| `schemas/header.schema.json` + `schemas/summary.schema.json` | drafted — header = versions + identities + test_plan + logging_config ($ref); summary = per-flow/radio/gps/upload stats (all optional). Examples validated. |
| `schemas/device_status.schema.json` | drafted — POST /status heartbeat: logger_state, current_test_id, logging_config_version, battery, storage, pending_uploads, radio snapshot (remediation 3.6). Examples validated. |
| `openapi/` | pending — wire `utoipa` into `server/` in Phase 1; replaces `planning/auth_openapi_sketch.yaml` |
| `testvectors/auth_signing/` | pending — cross-platform ECDSA round-trip vector |
