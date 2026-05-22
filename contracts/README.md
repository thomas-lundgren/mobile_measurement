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
| `schemas/` | `header.json`, `summary.json`, capability, logging-config | JSON Schema | DTO derivation; tolerant readers in analysis |
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
| `versions/versions.yaml` | drafted (this commit) |
| `enums/enums.yaml` | drafted (this commit) |
| `packets/` | pending — `core/packet.rs` already implements the 20-byte probe codec; promote to a layout table + golden vectors |
| `schemas/`, `parquet/` | pending — formalise from `core/log_writer.rs` |
| `openapi/` | pending — wire `utoipa` into `server/` in Phase 1; replaces `planning/auth_openapi_sketch.yaml` |
| `testvectors/auth_signing/` | pending — cross-platform ECDSA round-trip vector |
