# Phase 1 — Server skeleton + protocol loopback: detailed design

> Companion to `development_plan.md` (Phase 1 scope) and `development_guide.md`
> (symmetry / contract rules). Authoritative behaviour: the architecture spec.
> All wire/log/DB shapes are already pinned in `contracts/` — Phase 1 turns
> them into running Rust and proves the control plane + probe end-to-end on
> localhost, with no Android involved.

## 1. Goals & exit criteria

**Goal:** a working server-side control plane and a UDP-probe data path,
exercised end-to-end by a synthetic Rust client that speaks the real device
contract (enroll → challenge → verify → register → poll → run probe → status →
upload).

**Exit criteria (all on localhost, no Android):**

1. A synthetic client enrolls (P-256 keypair), authenticates by challenge/
   response, and registers capabilities.
2. An operator call (or test fixture) starts a probe-only test; `test-control`
   allocates a port, brings a `data-generator` to READY, and `server-main`
   enqueues `RUN_TEST`.
3. The client polls the command, runs the probe flow against the generator
   (UE-initiated), and both nodes write **segmented Parquet** matching the
   `contracts/parquet/` schemas.
4. The client reports `COMPLETED` via the upload protocol; the server assembles
   the `session/test/...` tree and the DB reflects the final state.
5. `contracts/openapi/openapi.yaml` is generated from the server and CI fails
   on drift; packet golden vectors and the auth-signing vector pass in Rust.
6. `cargo test --workspace` green; a single `./scripts/e2e_probe.sh` runs the
   whole flow.

Realtime-UDP and bulk-TCP generators are implemented after probe (sub-phase
1j), reusing the same generator/logger machinery.

## 2. Workspace layout

Extend the existing Cargo workspace. `core` stays the shared engine (also built
for Android); the server processes are separate crates so they can be separate
OS processes (crash isolation, per `contracts/internal/`).

```
Cargo.toml                      # workspace
core/                           # shared: packet codec, timestamp, engine, logger sink, auth-verify
server/
  contracts-rs/                 # Rust DTOs + enums + version consts; utoipa ToSchema; the codegen target
  server-main/                  # axum REST, auth, DB (sqlx), command queue, upload handler, OpenAPI emit
  test-control/                 # per-device state machine, port allocation, generator coordination
  data-generator/               # generator state machine + traffic engines (wraps core), internal API
  common/                       # shared server utilities: config, error type, internal HTTP client, tracing init
analysis/                       # (Phase 4) untouched
scripts/
  e2e_probe.sh                  # spins up the three processes + synthetic client
```

Rationale for `contracts-rs` as its own crate: the DTOs/enums are imported by
all three server binaries **and** are the source utoipa reads to emit OpenAPI.
Keeping them in one crate avoids cyclic deps and gives codegen a single home.

## 3. Dependencies (server side)

| Concern | Crate | Notes |
|---|---|---|
| async runtime | `tokio` | already in deps |
| HTTP server/client | `axum`, `reqwest` | axum for public REST + internal API; reqwest for internal client + synthetic client |
| OpenAPI | `utoipa`, `utoipa-swagger-ui` | code-first; `ApiDoc::openapi()` → YAML |
| DB | `sqlx` (sqlite, macros, migrate) | compile-time-checked queries; `db/schema.sql` → first migration |
| crypto (verify) | `p256`, `ecdsa`, `spki`, `sha2` | ECDSA-P256-SHA256 over SPKI DER pubkeys / ASN.1-DER sigs |
| operator JWT | `jsonwebtoken` | HS256 (MVP), issuer-swappable |
| password hash | `argon2` | operator login |
| IDs | `ulid` | prefixed: `dev_`/`tst_`/`ses_`/`upl_`/`chl_`/`opr_` |
| real-time ring | `rtrb` | wait-free SPSC (generator → logger) |
| thread prio/affinity | `thread-priority`, `core_affinity` | applied to generator threads |
| serde / json | `serde`, `serde_json` | already in deps |
| tracing | `tracing`, `tracing-subscriber` | structured logs |
| errors | `thiserror` | typed errors |

`core` already pulls `arrow`/`parquet`/`jni`/`libc`/`chrono`; add `p256`/
`ecdsa`/`spki`/`sha2` there (auth-verify is shared with future Android-side
needs and keeps crypto in one place) and `rtrb` for the sink.

## 4. Shared-core changes (sub-phase 1b)

These reconcile the Stage-A spike code to the now-frozen contracts.

### 4.1 Packet codec → framed `(pkt_t, ver)`

`core/src/packet.rs` currently encodes an unframed 20-byte probe. Rework to the
`contracts/packets/` framing:

- Add a 2-byte header `{ pkt_t: u8, ver: u8 }`; `ProbePacket` = `pkt_t=0x01,
  ver=0x01`, 20-byte core; `RealtimePacket` = `pkt_t=0x02, ver=0x01`, 28-byte core.
- A per-`(pkt_t, ver)` size table; decode reads the header, looks up `core_size`,
  rejects unknown `(pkt_t, ver)` with a typed error (not panic).
- Trailing bytes beyond the core are ignored (realtime padding).
- **Conformance test** loads `contracts/testvectors/packets/*.json` and asserts
  encode→hex and decode→fields byte-for-byte (the cross-language gate).

### 4.2 Logger sink → SPSC ring + full schema

Per `contracts/internal/logger_sink.md`:

- `LogSink<T>` = bounded **wait-free SPSC ring** (`rtrb`) of preallocated `Row`
  structs; `push(row) -> Result<(), Dropped>`; on full → drop + increment
  `dropped_rows` (never block).
- A `writer` thread drains the ring in batches → Arrow `RecordBatch` → segmented
  Parquet (zstd) — the existing `SegmentedPacketWriter`, generalized to the full
  `contracts/parquet/packets.parquet.yaml` column set (add `protocol`, ports,
  `sender_*`, `expected/actual_interval_ns`, join columns).
- Hot-path order is **timestamp → sendto → push** (documented + asserted by a
  unit test that checks send happens before the row is observable).
- One `packets` ring per flow thread; `events`/`clock`/`gps`/`radio` use the
  same sink type at low rate.

### 4.3 Auth verify primitive

Add `core::auth`:

- `verify_challenge(pubkey_spki_b64, server_id, challenge_id, nonce, sig_der_b64)
  -> bool` implementing the `"mp-auth-v1" || server_id || challenge_id || nonce`
  blob, `ECDSA_P256_SHA256`, ASN.1-DER sig, SPKI-DER pubkey.
- A signer counterpart (for the synthetic client + the auth-signing vector).
- **`contracts/testvectors/auth_signing/`** vector
  (`priv_key_pem, pub_key_spki_b64, message_hex, signature_der_b64`) is produced
  here and round-tripped by a core test — the DER-vs-IEEE-P1363 gate.

### 4.4 Spec doc cleanup

Reconcile spec §12 `t_mono_ns`/`t_utc_ns` → `mono_ns`/`utc_ns` to match the
contracts and the writer.

## 5. `contracts-rs` (sub-phase 1a)

The Rust face of `contracts/`.

- **Enums** — generated by a `build.rs` from `contracts/enums/enums.yaml` into
  Rust `enum`s with `serde(rename_all)` matching the wire strings. The generated
  file is committed; CI regenerates and asserts no diff (drift gate).
- **Version constants** — generated from `contracts/versions/versions.yaml`
  (`PROTOCOL_VERSION`, `SCHEMA_VERSION`, `AUTH_*`).
- **DTOs** — hand-authored serde structs mirroring the JSON Schemas
  (`EnrollRequest`, `ChallengeRequest/Response`, `VerifyRequest/Response`,
  `Capability`, `LoggingConfig`, `TestPlan`, `FlowBinding`, `RunTestPayload`,
  `CreateTestRequest`, `DeviceStatus`, `UploadManifest`, `UploadInitResponse`,
  internal `*` types, controller `DeviceListItem`/`TestStatus`). Each derives
  `serde` + `utoipa::ToSchema`.
- **Schema parity test** — for each DTO with a JSON Schema, a test validates the
  contract's example file deserializes into the DTO and re-serializes without
  loss. This keeps `contracts-rs` honest against `contracts/schemas/`.

## 6. `server-main` (sub-phases 1c–1e, 1h)

The single public, authenticated surface. Axum app over TLS (self-signed in
dev; mandatory in prod per the auth model).

### 6.1 Modules

```
server-main/
  main.rs            # config load, DB pool, router, TLS, serve
  config.rs          # bind addrs, server_host, port range hand-off to test-control, DB path, secrets
  db.rs              # sqlx pool; migrations; typed row structs
  error.rs           # AppError -> HTTP status + JSON body
  auth/
    operator.rs      # /auth/login, JWT issue/verify, argon2
    device.rs        # /devices/enroll, /auth/challenge, /auth/verify; session tokens
    middleware.rs    # extract+validate bearer; audience separation (operator vs device)
  registry.rs        # devices, capability, device_status
  commands.rs        # enqueue + poll + ack
  tests_api.rs       # operator: POST/GET /tests, stop; calls test-control internal API
  uploads.rs         # 3-phase upload handler; assembles the test tree
  openapi.rs         # #[derive(OpenApi)]; emit openapi.yaml; serve swagger-ui
```

### 6.2 Endpoints (device-facing, from the auth + contracts work)

| Method | Path | Auth | Body / result |
|---|---|---|---|
| POST | `/api/v1/devices/enroll` | enrollment token | `EnrollRequest` → `device_id` etc. |
| POST | `/api/v1/auth/challenge` | none | `{device_id}` → nonce |
| POST | `/api/v1/auth/verify` | none | signed blob → session token |
| POST | `/api/v1/devices/register` | session | `Capability` → effective `LoggingConfig` |
| GET | `/api/v1/devices/{id}/command` | session | next pending command or 204 |
| POST | `/api/v1/devices/{id}/command/{cmd_id}/ack` | session | mark delivered/acked |
| POST | `/api/v1/devices/{id}/status` | session | `DeviceStatus` (upserts `device_status`) |
| POST | `/api/v1/tests/{id}/summary` | session | `UploadManifest` → `UploadInitResponse` |
| PUT | `/api/v1/tests/{id}/upload/{upload_id}/files/{path}` | session | file bytes |
| POST | `/api/v1/tests/{id}/upload/{upload_id}/complete` | session | verify + assemble |

### 6.3 Endpoints (controller-facing)

`POST /api/v1/auth/login`, `GET /devices`, `GET /devices/{id}`, `POST /tests`,
`GET /tests`, `GET /tests/{id}`, `POST /tests/{id}/stop`,
`POST /devices/{id}/upload`, `PUT /devices/{id}/logging-config` — per
`planning/controller_openapi_sketch.yaml`. These translate to internal-API calls
and command-queue enqueues.

### 6.4 Identity rule

`device_id` is **always** derived from the validated session token, never from
path/body — middleware injects an authenticated principal; handlers read it from
there. Operator vs device token audiences are disjoint.

### 6.5 Command queue semantics

`GET /command` returns the oldest `pending` command for the device and marks it
`delivered`; the device acks after acting (`acked`). MVP uses **short polling**
(device polls every N s); the schema/handler leave room for long-poll later.
`RUN_TEST` payload is the assembled `RunTestPayload` (plan + flow_bindings +
server_host).

### 6.6 OpenAPI emit + drift gate

`openapi.rs` collects all `#[utoipa::path]` handlers + `ToSchema` DTOs.
`cargo run --bin server-main -- --emit-openapi > contracts/openapi/openapi.yaml`
(or a small xtask). CI runs the emit and `git diff --exit-code` on
`contracts/openapi/`. This file replaces the two `planning/*_openapi_sketch.yaml`,
which are then deleted.

## 7. `test-control` (sub-phase 1g)

Owns the per-device test state machine, port allocation, and generator
coordination. Exposes the internal control API (`contracts/internal/control_api.md`)
to `server-main`; acts as client to `data-generator`.

### 7.1 Internal API server (called by server-main)

`POST /internal/tests`, `GET /internal/tests/{id}`, `POST /internal/tests/{id}/stop`
(bodies = `internal_start_test` / `internal_test_status`). Loopback bind, no auth.

### 7.2 State machine

`IDLE → PREPARE → WAITING_FOR_GENERATOR → WAITING_FOR_CLIENT → RUNNING →
STOPPING → DONE|FAILED` (enum from `enums.yaml`). Transitions:

- **PREPARE:** allocate one port per flow from a configured range (e.g.
  41000–41999), in-memory allocator backed by the `flows` table; for each flow
  `POST /internal/generators` to a data-generator with the port-less `Flow` +
  allocated `server_port` + `node_log_dir`.
- **WAITING_FOR_GENERATOR:** poll `GET /internal/generators/{id}` until all
  report `READY` (bound + listening).
- **WAITING_FOR_CLIENT:** report bindings up via `internal_test_status`;
  server-main enqueues `RUN_TEST`. Generators auto-advance to RUNNING when the
  UE initiates (invariant 5).
- **RUNNING → STOPPING:** on duration elapse or `STOP_TEST`, `POST
  /internal/generators/{id}/stop` each; collect final counters.
- **DONE/FAILED:** compute `result` (incl. PARTIAL if any `dropped_rows > 0`);
  release ports.

### 7.3 Port allocation

Single source of truth = `test-control` process memory + the `flows` rows it
writes. Range from config; refuse to start a test that can't be fully bound
(409 back to server-main). Ports released on terminal state.

## 8. `data-generator` (sub-phases 1f, 1j)

A thin process around `core`'s engine + logger sink, driven by the internal API.
On the server it is the symmetric peer of the mobile core.

### 8.1 Internal API server (called by test-control)

`POST /internal/generators` (`internal_generator_prepare`),
`GET /internal/generators/{id}` (`internal_generator_status`),
`POST /internal/generators/{id}/stop`. Loopback, no auth.

### 8.2 Generator state machine & threads

`INITIALIZING → READY → WAITING_FOR_CLIENT → RUNNING → STOPPING → STOPPED|FAILED`.
On prepare: bind the socket, open the logger sink (writes into `node_log_dir`),
spawn the flow thread (pinned + raised priority), reach READY. The flow thread
derives its role from `(flow.type, flow.direction)`:

- **probe (1f):** echo — `recvfrom` → log RECEIVED → `sendto` back verbatim →
  log SENT. (Server hosts the echo; UE originates.)
- **realtime_udp (1j):** FromServer → send at `rate_pps`, padded to
  `packet_size_bytes`, embedding dual-clock; ToServer → receive + log
  `sender_*` for OWD.
- **bulk_tcp (1j):** accept the UE-initiated connection; stream/sink bytes per
  direction; bidirectional is two flows/connections.

Counters (`packets_sent/received`, `bytes_*`, `dropped_rows`) are surfaced via
the status endpoint and rolled into `summary.json` at stop.

### 8.3 Logging

Uses the `core` logger sink (§4.2): the flow thread is the SPSC producer; a
writer thread drains to `server/{packets,events,clock}.parquet` + `summary.json`
for the test. No `gps`/`radio` on the server.

## 9. Synthetic test client (sub-phase 1i)

A Rust binary (`server/test-client` or under `scripts/`) that behaves like the
device control plane so Phase 1 is provable without Android:

1. Generate a P-256 keypair (Keystore/Enclave is mocked by an in-memory key);
   `POST /devices/enroll` with an enrollment token.
2. `challenge` → sign the blob with `core::auth` → `verify` → session token.
3. `register` with a Pixel-9-Pro-like `Capability`; receive `LoggingConfig`.
4. Poll `command`; on `RUN_TEST`, run the probe flow via `core`'s engine
   (UE-initiated to the generator port), writing `mobile/*.parquet`.
5. `POST /status` heartbeats throughout.
6. On completion, run the 3-phase upload; assert the server assembled the tree.

This client doubles as the **interop fixture** for Phase 2 (the Android Logger
must produce byte-identical Parquet + the same control-plane sequence).

## 10. End-to-end probe sequence (the exit demo)

```
client          server-main         test-control        data-generator
  │ enroll ─────────▶ store pubkey                          
  │ challenge ──────▶ nonce                                  
  │ verify(sig) ────▶ session token                         
  │ register ───────▶ effective LoggingConfig               
  (operator/fixture) POST /tests ──▶ /internal/tests ──▶ POST /internal/generators
                                      allocate port,        bind, READY
                                      poll READY ◀──────────  
                     enqueue RUN_TEST◀ bindings              
  │ GET command ◀──── RUN_TEST(payload)                      
  │ probe: sendto generator port ───────────────────────▶ echo + log
  │ ...RTT samples, both sides log segmented Parquet...      
  │ status heartbeats ▶                                      
  (duration elapses) /internal/tests state ▶ STOPPING ─▶ stop generator (flush)
  │ summary+manifest ▶ upload_id + needed_paths             
  │ PUT files ▶ ... ▶ verify hashes                          
  │ complete ───────▶ assemble session/test tree, DB DONE   
```

## 11. Cross-cutting: config, errors, observability

- **Config** (`server/common`): figment/env + a `server.toml` — bind addresses,
  `server_host` advertised to devices, generator port range, DB path, JWT
  secret, enrollment-token policy, TLS cert/key (optional in dev).
- **Errors:** one `AppError` (`thiserror`) per process → mapped to HTTP status +
  a small JSON `{error, detail}`; never leak internals.
- **Tracing:** `tracing` spans keyed by `test_id`/`device_id`/`flow_id`; the
  three processes log to stdout; `e2e_probe.sh` tees them.

## 12. Testing strategy

| Layer | What |
|---|---|
| unit | packet codec, auth verify/sign, port allocator, state-machine transitions, ring drop+count |
| contract | golden vectors (packets, auth_signing); `contracts-rs` DTOs ↔ `contracts/**` example files; OpenAPI drift gate |
| schema | `db/schema.sql` applies; sqlx compile-time query checks against it |
| integration | each internal API hop with an in-process stub peer |
| end-to-end | `scripts/e2e_probe.sh`: real three processes + synthetic client → assert COMPLETED, tree assembled, Parquet readable in pyarrow + arrow-rs |
| forward-compat | a Parquet written by the writer reads back with an extra unknown column ignored / missing column null |

## 13. CI gates (added this phase)

- `cargo build/test --workspace`, `cargo clippy -D warnings`, `cargo fmt --check`.
- **Contract drift:** regenerate enums/versions + emit OpenAPI → `git diff
  --exit-code contracts/`.
- **Golden vectors:** packet + auth-signing conformance.
- **Additive-only schema gate:** diff `contracts/parquet` + JSON Schemas against
  the base branch; reject column/field removal without a `schema_version` bump.
- **e2e probe** job runs `scripts/e2e_probe.sh`.

## 14. Sub-phase sequencing (PR-sized)

1. **1a** workspace + `contracts-rs` (enum/version codegen, DTOs, parity tests).
2. **1b** core reconciliation: framed packets + golden vectors; SPSC sink + full
   packets schema; auth verify/sign + auth-signing vector; spec §12 rename.
3. **1c** server-main DB layer (sqlx, migration from `db/schema.sql`, registry).
4. **1d** auth: enroll/challenge/verify + session middleware + operator login.
5. **1e** command queue + device endpoints (register/command/status).
6. **1f** data-generator probe echo + internal API + logger sink.
7. **1g** test-control: state machine, port allocation, generator coordination.
8. **1h** upload handler (3-phase) + tree assembly.
9. **1i** synthetic client + `e2e_probe.sh` + OpenAPI emit/drift gate → **exit
   criteria met for probe**.
10. **1j** realtime UDP + bulk TCP generators (extends 1f machinery).

## 15. Open decisions to confirm before/while building

- **TLS in dev:** self-signed + client trust-store, or plain HTTP on loopback
  for Phase 1 with TLS deferred to a hardening pass? (Spec says TLS mandatory
  externally; Phase 1 is localhost.)
- **JWT alg:** HS256 (shared secret, simplest) vs ES256 (asymmetric, SSO-ready).
  MVP leans HS256.
- **Process supervision:** `e2e_probe.sh` launches three binaries for the demo;
  is a longer-term supervisor (systemd units / a parent process) in scope for
  Phase 1 or deferred to Phase 5?
- **Generator process granularity:** one `data-generator` process per test vs a
  long-lived pool that handles many flows/tests. MVP: one process, one flow
  thread per flow; revisit under fleet load.
- **`actual_interval_ns` for probe:** defined for realtime; for probe leave null
  (RTT is the probe metric) — confirm.
