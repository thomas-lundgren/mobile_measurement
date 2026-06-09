# Measurement Platform

A distributed measurement platform for real-world mobile-network (4G/5G)
experiments. Android devices move through live networks while a Linux server
generates and receives controlled traffic; both ends record dual-clock,
per-packet logs. The goal is to characterise **latency, one-way delay, jitter,
loss, throughput, and handover behaviour**, correlated with **GPS position and
radio metrics**.

> Status: an end-to-end MVP across all planned phases (server, Android Logger,
> Android Controller, analysis toolchain, hardening), verified on a physical
> Pixel 9 Pro over USB and LAN. See [`ROADMAP.md`](ROADMAP.md) for the phase plan
> and [`STATUS.md`](STATUS.md) for detailed point-in-time progress.

## How it works

```
Controller (Android) ──▶ server-main ──▶ test-control ──▶ data-generator(s)
   operator UI            (Rust)          (Rust)            (Rust, shared core)
                              ▲
                              │ REST: enroll / challenge / poll / status / upload
                              │
                       Logger (Android, Java shell + shared Rust core via JNI)
```

- **Linux server** (Rust): `server-main` (control plane — REST, device registry,
  command queue, SQLite, uploads), `test-control` (scheduler, port allocation,
  per-device state machines, generator coordination), `data-generator(s)`
  (UDP probe, realtime UDP, bulk TCP).
- **Android Logger** — autonomous measurement client: a foreground service that
  drives the **shared Rust core** over JNI (traffic engine + Parquet logger),
  samples GPS/radio, and uploads sealed logs. Talks only to `server-main`.
- **Android Controller** — operator console (Kotlin): logs in, lists the fleet,
  starts/stops tests, watches live state. Performs no measurement; its REST
  client is generated from the OpenAPI contract.
- **Python analysis toolchain** (`analysis/`) — tolerant Parquet loaders;
  RTT / OWD-with-uncertainty / jitter / loss; derived radio events; plots.

The same Rust **core** (`fieldtest-core`) is linked natively into the server and
cross-compiled (`cargo-ndk`) into the Logger, so both nodes share one traffic
engine, one Parquet writer, and one wire codec — no second implementation.

## What you can measure

### Traffic flows (defined per test, mixed freely in one run)

| Flow            | Direction(s)              | What it measures                                                 |
|-----------------|---------------------------|------------------------------------------------------------------|
| `probe`         | bidirectional echo        | RTT, jitter, packet loss; cheap heartbeat                        |
| `realtime_udp`  | `FromServer` / `ToServer` | one-way delay (OWD), inter-arrival jitter, loss at fixed PPS     |
| `bulk_tcp`      | `FromServer` / `ToServer` | sustained throughput (UL or DL), with per-chunk arrival logging  |
| `gateway_ping`  | local (UE → first hop)    | first-hop latency to the auto-discovered gateway (rmnet / wlan)  |
| `dscp_probe`    | bidirectional, marked     | DSCP transparency UL and DL independently (which bits survive)   |

Every flow is logged on **both** endpoints into per-packet Parquet, with
embedded sender timestamps where applicable so OWD/jitter can be reconstructed
offline.

### Metrics derived (in `analysis/`)

- **RTT** and inter-arrival jitter from `probe`
- **One-way delay** with explicit **clock-uncertainty bounds** from
  `realtime_udp`
- **Packet loss** and sequence-gap analysis on any UDP flow
- **Throughput** (bytes/sec, p50/p95) from `bulk_tcp`
- **DSCP transparency matrix** — which DSCP values the operator preserves vs.
  remarks vs. strips, separately for UL and DL
- **Handover events**, **coverage gaps**, **band changes** — derived
  post-hoc from `radio.parquet` (PCI/NCI/band transitions), not logged live

### Contextual signals (recorded by the Logger, once per test)

- **GPS** position track (`gps.parquet`)
- **Radio**: serving cell (PCI, NCI, TAC, NRARFCN, RSRP, RSRQ, SINR),
  neighbor cells, LTE fallback (`radio.parquet`)
- **Clock-sync samples** (`clock.parquet`) — mono ↔ utc ↔ gps offsets for
  OWD uncertainty estimation
- **Capability-tolerant**: anything the Pixel 9 Pro doesn't expose
  (DRX, 5QI/QFI, MCS/CQI/BLER/HARQ, RRC-state) is silently omitted; readers
  treat all fields as optional

### Operating modes

- **Logger** — test-driven: the device polls for `RUN_TEST`, drives one or
  more flows, seals the resulting log dir, uploads when convenient.
- **Monitor** — always-on background probing (server-configured pings to
  pinned targets) that runs in parallel with tests; suitable for continuous
  network-quality coverage rather than scheduled experiments.

### Platform-level capabilities

- **Hardware-backed device identity** — P-256 enrollment + challenge/response
  with operator-issued one-time enrollment tokens
- **Operator console (Android Controller, Kotlin)** — fleet view, test
  start/stop, live state; REST client generated from OpenAPI
- **Server-configured logging** — effective config =
  *test-plan profile ∩ device capabilities ∩ overrides*, versioned, pushed
  via `SET_LOGGING_CONFIG`
- **Logging-first** — local Parquet writes never block on the network;
  uploads are deferred and resumable (per-file SHA-256 manifests)
- **Forward-compatible logs** — additive-only schemas; readers tolerate
  unknown fields and null cells
- **Cross-language contract gates** — single `contracts/` directory with
  OpenAPI / JSON Schema / Parquet schemas / golden binary vectors /
  SQL schema, all parity-tested in CI

## Cross-cutting invariants

These span components and are honoured everywhere:

1. **Logging-first** — data is written locally first; upload is secondary and
   never blocks or interrupts an active measurement.
2. **Dual-clock timestamps** — every record carries `mono_ns` (authoritative for
   ordering/jitter) and `utc_ns` (cross-node correlation / OWD). OWD estimates
   carry clock-uncertainty bounds.
3. **Server-configured logging** — the UE logs only what the server enables
   (effective config = test-plan profile ∩ device capabilities ∩ overrides),
   versioned and pushed via `SET_LOGGING_CONFIG`.
4. **Forward-compatible logs** — all fields optional; additive-only; unknown
   fields tolerated.
5. **UE always initiates** — the mobile opens every connection (NAT/firewall
   punch-through).
6. **Symmetric peers** — both nodes generate, receive, and log; a flow's
   *direction* only decides which side sends payload.

## Repository layout

```
core/         shared Rust engine + Parquet logger (native + Android .so via JNI)
server/       Rust workspace crates:
  contracts-rs/   DTOs + enums (utoipa ToSchema) — the Rust face of contracts/
  server-main/    control-plane REST API, registry, uploads, SQLite
  test-control/   scheduler, port allocation, generator coordination
  data-generator/ UDP probe / realtime UDP / bulk TCP
android/      Gradle: logger/ (Java + core via JNI) + controller/ (Kotlin)
analysis/     Python package (fieldtest_analysis): loaders, metrics, plots, CLI
contracts/    single source of truth: OpenAPI, JSON Schemas, Parquet schemas,
              golden vectors, enums, DB schema
ci/           local gate (check.sh) + contract & client-drift validators
planning/     authoritative architecture spec + per-phase design docs + runbook
```

## Build & test

### Rust server + core

```bash
cargo build                 # all crates
cargo test --workspace      # unit/integration tests + contract gates
ci/check.sh                 # full local gate (fmt, clippy, tests, contracts,
                            # analysis pytest, generated-client drift)
```

`cargo test` also enforces the contract gates: enum/version parity vs
`contracts/*.yaml`, packet + auth-signing golden vectors, OpenAPI drift vs
`contracts/openapi/openapi.yaml`, DB-migration drift vs `contracts/db/schema.sql`,
and a full probe end-to-end.

### Analysis toolchain

```bash
cd analysis
python3 -m venv .venv && .venv/bin/pip install -e ".[dev,plots]"
.venv/bin/python -m pytest -q
.venv/bin/python -m fieldtest_analysis <test_dir> --plots out/   # summary + PNGs
```

### Android

Requires the Android SDK + NDK, Gradle 8.13, and a `local.properties` with
`sdk.dir`/`ndk.dir` (no Gradle wrapper is committed).

```bash
android/build_apk.sh                              # Logger: cargo-ndk core → APK
(cd android && gradle :controller:assembleDebug)  # Controller (Kotlin)
android/controller/generate_client.sh             # regen REST client from OpenAPI
```

## Run a test end-to-end

The full local/on-device bring-up — starting the three servers, minting an
operator + enrollment token with `mp-admin`, onboarding a device, and driving a
test — is documented step by step in
[`planning/device_bringup.md`](planning/device_bringup.md). In short: the device
enrolls and authenticates with a hardware-backed P-256 key, the operator posts a
port-less test plan, the server allocates ports and pushes `RUN_TEST`, both nodes
run + log the flows, and the mobile uploads its sealed Parquet for analysis.

## Contracts & cross-language consistency

`contracts/` is the single source of truth for every cross-component boundary.
The OpenAPI document is **generated from the Rust server** (`utoipa`) and
drift-gated; enums/versions come from one YAML; the Controller's Kotlin client
and (planned) other clients are generated from the OpenAPI and drift-gated in CI;
golden vectors pin the wire formats across languages. See
[`planning/development_guide.md`](planning/development_guide.md).

## CI

`.github/workflows/ci.yml` runs: Rust fmt + clippy (`-D warnings`) + workspace
tests, contract-example validation, the analysis pytest suite, and the
generated-client drift gate. Mirror it locally with `ci/check.sh`.

## Documentation

- [`docs/OPERATORS_GUIDE.md`](docs/OPERATORS_GUIDE.md) — set up, configure, and operate every part of the system (server, Logger, Controller, analysis).
- [`planning/device_bringup.md`](planning/device_bringup.md) — copy-paste single-host end-to-end bring-up.
- [`planning/5_g_field_test_platform_architecture_spec.md`](planning/5_g_field_test_platform_architecture_spec.md) — authoritative architecture & functional spec (packet formats, log schemas, OWD math, endpoints).
- [`planning/development_plan.md`](planning/development_plan.md) — build sequencing + per-invariant enforcement.
- [`planning/development_guide.md`](planning/development_guide.md) — keeping the polyglot codebase consistent (contracts, codegen, golden vectors).
- [`planning/peer_topology_extension.md`](planning/peer_topology_extension.md) — proposed extension: generalised peer-endpoint registry (any node as a peer, UE↔UE topologies) + deployment behind a home router (NAT/port forwarding).
- [`AGENTS.md`](AGENTS.md) / [`CLAUDE.md`](CLAUDE.md) — agent-oriented guides.
- [`ROADMAP.md`](ROADMAP.md) · [`STATUS.md`](STATUS.md) — phase plan & progress.

## Reference device & scope

Reference device is the **Pixel 9 Pro**. DRX, 5QI/QFI, MCS/CQI/BLER/HARQ, and
RRC-state are not exposed without root and are out of MVP scope; handovers are
derived retrospectively from PCI/NCI changes. The architecture is intentionally
built to grow from a "test instrument" into a continuous network-quality probe
without a rewrite (see `planning/long_term_traffic_monitoring.md`).
