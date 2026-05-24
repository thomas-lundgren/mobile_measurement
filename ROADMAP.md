# Roadmap — 5G Field Test Platform

> Project plan overview. For point-in-time progress see `STATUS.md`; for the
> authoritative architecture see `planning/5_g_field_test_platform_architecture_spec.md`;
> for build sequencing and per-invariant enforcement see `planning/development_plan.md`.

## What it is

A distributed measurement platform for real-world 5G experiments: Android
devices move through live mobile networks while a Linux server generates and
receives controlled traffic. The goal is to characterize latency, jitter,
loss, one-way delay, throughput, and handover behavior — correlated with GPS
position and radio metrics.

## Components

```
Controller (Android) ──▶ server-main ──▶ test-control ──▶ data-generator(s)
   operator UI            (Rust)          (Rust)            (Rust, shared core)
                              ▲
                              │ REST (enroll/challenge/poll/status/upload)
                              │
                       Logger (Android Java shell + shared Rust core via JNI)
```

- **Linux server** (Rust): `server-main` (REST, registry, command queue, DB,
  uploads), `test-control` (scheduler, port allocation, state machines,
  generator coordination), `data-generator(s)` (UDP probe, realtime UDP, bulk TCP).
- **Android Logger** — autonomous measurement client; foreground service + the
  shared Rust core via JNI.
- **Android Controller** — operator remote-control / monitoring UI; performs no
  measurement.
- **Python analysis toolchain** — tolerant Parquet loaders, OWD/RTT/jitter/loss,
  route & radio correlation plots, derived handover events.

## Cross-cutting invariants (hold across all phases)

1. **Logging-first** — data written locally first; upload never blocks measurement.
2. **Dual-clock timestamps** — every record carries `mono_ns` (ordering) +
   `utc_ns` (correlation / OWD).
3. **Server-configured logging** — UE logs only what the server enables
   (`effective ⊆ capability`).
4. **Forward-compatible logs** — all fields optional, additive-only, unknown
   fields tolerated.
5. **UE always initiates** — the mobile opens every connection (NAT punch-through).
6. **Symmetric peers** — both nodes generate + receive + log; direction only
   selects who sends payload.

## Phase plan & status

| Phase | Scope | Status |
|---|---|---|
| **Step 0 — JNI viability spike** | Prove shared Rust core over JNI: sockets, dual clock, segmented Parquet, crash survival, APK size — Stage A (desktop JVM) + Stage B (Pixel 9 Pro) | ✅ Complete — all checks green on physical device |
| **Phase 0 — Foundations & contracts** | `contracts/` single source of truth for every cross-component boundary | ✅ Complete |
| **Phase 1 — Server skeleton + protocol loopback** | `server-main` (axum/utoipa/sqlx), `test-control` (state machines, port alloc, READY gating), `data-generator` (probe→realtime→bulk), full probe e2e test | ✅ Complete |
| **Phase 2 — Android Logger MVP** | Foreground service, registration, command poller, logger state machine, logging-first store + upload manager, traffic engine via JNI core, radio/GPS sampling | 🔶 In progress — 2a–2e done (UE engine, server interop, RustCore JNI + aux writers, full app builds to an APK); only 2f (on-device Pixel run) remains |
| **Phase 3 — Android Controller MVP** | Auth, generated API client, device list, start/stop test, upload control, dashboard | ⬜ Can start (Kotlin client from openapi.yaml) |
| **Phase 4 — Analysis toolchain** | Tolerant loaders, OWD-with-uncertainty, RTT/jitter/loss, plots, derived handover/coverage events | ⬜ Runs parallel to P2 |
| **Phase 5 — Hardening & field readiness** | Failure handling, multi-device fleet, dynamic config, clock-uncertainty validation, battery/storage, chaos tests | ⬜ Final |

**Parallelism:** after Phase 1, Phases 2 (Android) and 4 (Analysis) run in
parallel — both pinned to the contracts and fixtures.

## Phase 0 — the contract layer (done)

All in `contracts/`, each validated (JSON Schema / hex round-trip / SQL execution):

- **versions, enums** — version + auth constants; commands, three state
  machines, flow types, event types, results.
- **wire packets** — Probe v1 + Realtime v1, `(pkt_t, ver)` framing, golden vectors.
- **test_plan / RUN_TEST** — port-less plan + `flow_bindings`.
- **capability / logging_config** — `effective ⊆ capability`.
- **upload protocol** — 3-phase resumable per-file + test-result rules.
- **log schemas** — 5 Parquet tables + header/summary JSON.
- **device status, controller REST, internal control API + logger-sink, DB
  schema** (SQLite, 12 tables).

## Architecture decisions settled

- **Shared Rust core via JNI** — one engine+logger implementation linked into
  both server (native) and Android (`cargo-ndk` → `.so`). No second codec.
- **Parquet writer in the core** — identical Parquet on both nodes; segmented
  for crash survival.
- **Code-first contracts** — REST OpenAPI generated from Rust (`utoipa`);
  enums/versions from single YAML; golden vectors + interop matrix +
  additive-only schema gate in CI.
- **Monorepo** — one repo, one commit builds server + Android core; path-based
  CI triggers; independent release tagging.
- **Device auth** — enrollment + per-session challenge/response over a
  non-exportable P-256 keypair (Android Keystore / iOS Secure Enclave),
  `ECDSA_P256_SHA256` / SPKI / ASN.1 DER pinned cross-platform; operator auth =
  username/password → JWT.
- **Wire formats grow by version bump**, not runtime extensibility (no TLV tails).
- **Real-time data path** — generator + logger = one process / separate threads
  on both nodes (forced on mobile by JNI); SPSC ring handoff with drop+count
  overflow; internal control IPC = local HTTP+JSON (not gRPC).
- **server-main DB** = SQLite (WAL), control-plane metadata only; measurement
  data stays as Parquet on the filesystem, referenced by path.

## Phase 1 entry (immediate next steps)

1. Wire `axum` + `utoipa` + `sqlx` into `server/` → emits authoritative
   `contracts/openapi/openapi.yaml` (replaces the `planning/*_openapi_sketch.yaml`),
   applies `db/schema.sql` as the first migration, implements the device control
   plane + internal API.
2. Reconcile `core/packet.rs` (→ framed `(pkt_t, ver)` layout) and
   `core/log_writer.rs` (→ full packets schema); add the SPSC logger ring per
   `contracts/internal/logger_sink.md`.
3. Add the `auth_signing` golden vector with the Rust verifier (pins the
   DER-vs-IEEE-P1363 interop bug).
4. Doc cleanup: reconcile spec §12 `t_mono_ns` → `mono_ns`.

## Reference device & MVP scope

- **Pixel 9 Pro** is the reference. DRX, 5QI/QFI, MCS/CQI/BLER/HARQ, RRC-state
  are out of MVP scope (not exposed without root). Handovers are derived
  retrospectively from PCI/NCI changes.
- **iOS** is contract-neutral (the auth scheme works cross-platform) but out of
  MVP implementation.

## Long-term direction

The MVP "test instrument" is intentionally built to grow into a **continuous
network-quality probe** (logging real app traffic, monitoring latency to
arbitrary destinations) without rewrite — the shared core, segmented Parquet,
dual clocks, and server-configured logging all carry forward. See
`planning/long_term_traffic_monitoring.md`.
