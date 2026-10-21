# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Current state

Implemented and running live (home server + one Pixel 9 Pro). `STATUS.md`
has the history; the design documents are still authoritative:

- `planning/5_g_field_test_platform_architecture_spec.md` — the architecture & functional spec. Update it first for any design change.
- `planning/development_plan.md`, `planning/development_guide.md` — contract-first build discipline (single-source contracts, golden vectors, interop tests).
- `planning/ota_and_versioning_plan.md` — planned (parked) over-the-air updates and versioned logs.

### Layout

| Path | What |
|---|---|
| `core/` | Shared Rust engine (rlib for the server, cdylib + JNI for Android): traffic engines used by both nodes — `endpoint` (realtime UDP / bulk TCP loops) and `exchange` (request/response: `transfer` on the UE, `serve` on the server); `ue` (UE engine); `app_traffic` workloads (`model` = simulated users, `runner` = agents, pools, bursts over `exchange`); `log_writer` (SPSC ring → segmented Parquet), `udp_timing` (kernel timestamps, TOS, pacing), `packet`, `aux_log`, `auth` |
| `server/` | `contracts-rs` (DTOs), `server-main` (REST, auth, uploads, `/viz` pages), `test-control` (scheduling, ports), `data-generator` (server flow threads) |
| `android/logger` | Logger app (Java + RustCore JNI); `android/build_apk.sh` builds core + APK. `android/controller` (Kotlin, generated client) |
| `probe/` | `mp-probe`, adb-started on-phone collector (modem qdisc, UDP counters, radio log) on 127.0.0.1:47600 |
| `analysis/` | Python package `fieldtest_analysis` (loader, metrics, series/findings for the pages, app/timing/spectral) |
| `contracts/` | Single source of truth: JSON schemas, OpenAPI, Parquet YAML, packet docs, golden vectors, versions |
| `scripts/` | `viz_rebuild.sh` (cron page data), `fieldtest/` (cycle scripts on `lib.sh`, commute watcher) |

### Commands

```bash
cargo build --workspace                      # Rust (release: --release; servers run target/release/*)
cargo test --workspace                       # all Rust tests; one: cargo test -p fieldtest-core <name>
ci/check.sh                                  # fmt, clippy -D warnings, tests, contracts, analysis, client drift
analysis/.venv/bin/python -m pytest -q analysis/tests     # analysis tests (run from repo root or analysis/)
android/build_apk.sh                         # cross-compile core (cargo-ndk) + Logger debug APK
cargo ndk -t arm64-v8a build -p mp-probe --release        # mp-probe for the phone
```

Live deployment notes (paths, restart patterns, phone quirks) are in the
auto-memory, not here: the servers run from `~/mp-data`, cron runs
`scripts/viz_rebuild.sh` and `scripts/fieldtest/*`.

## What this is

A distributed measurement platform for real-world 5G experiments: Android devices move through live mobile networks while a Linux server generates/receives controlled traffic. Three components:

- **Linux server** (planned Rust: axum/tokio/serde/sqlx/tracing) — three processes: `server-main` (control plane: device registry, REST API, command queues, upload manager), `test-control` (scheduler, port allocation, per-device state machines, generator coordination), `data-generator(s)` (UDP probe, realtime UDP, bulk TCP).
- **Android Logger app** — autonomous measurement client; foreground service; also implements traffic generator/receiver.
- **Android Controller app** — remote-control/monitoring UI; performs no measurements.

Control flow: `Controller → server-main → test-control → data-generators`. Logger clients talk **only** to `server-main`.

## Cross-cutting invariants (any implementation must honor these)

These are the design decisions that span components and are easy to violate piecemeal:

1. **Logging-first.** Data is always written locally on the device first; upload is strictly secondary and must **never** interrupt or block an active measurement. Tests continue through network loss; uploads are deferred.

2. **Dual-clock timestamps everywhere.** Every timestamp carries both `t_mono_ns` (monotonic — authoritative for ordering, jitter, inter-arrival, bursts) and `t_utc_ns` (for cross-device/server correlation and one-way delay). Don't collapse these into one clock. OWD estimates must carry clock-uncertainty bounds.

3. **Server-configured logging.** The UE logs only what the server enables. The effective logging config = intersection of test-plan profile and device-reported capabilities, with per-device overrides applied last. Config is versioned (`logging_config_version`); the server pushes updates via the `SET_LOGGING_CONFIG` command.

4. **Capability-tolerant, forward-compatible logs.** Reference device is the **Pixel 9 Pro**; DRX, 5QI/QFI, MCS/CQI/BLER/HARQ, and RRC-state are **not available** on it and are out of MVP scope. All log fields are optional at the row level; new fields may be added without bumping `schema_version`; missing fields read as `null`; readers must tolerate unknown fields. Handovers are derived retrospectively from PCI/NCI changes for baseline devices.

5. **UE always initiates the connection.** For every flow direction (`FromServer`/`ToServer`/`Bidirectional`), the mobile opens the TCP connection or sends the first UDP packet to the configured server port (NAT/firewall punch-through). For `FromServer` UDP the UE sends ~1 Hz keepalives until the first downlink packet arrives. During tests the UE is forced to **RRC_CONNECTED active** (DRX out of scope).

6. **Both endpoints are symmetric peers.** Server and mobile each implement generator + receiver + **loopback** + logger, both participate in measurement, and **both produce log files**. The configured *direction* only decides which side sends payload (the UDP probe is the loopback/echo case — either node may host the echo). A **test** aggregates both nodes' recordings under one `test_id` and may hold many flows; GPS and the radio metric trace are recorded **once per test** (mobile-only), shared by all flows.

7. **Single-source contracts.** Every value that crosses a component or language boundary is defined **once** in `contracts/`: packet layouts, REST/JSON bodies, enum and state strings, log columns, version constants, and side protocols (reach, mp-probe). Every other copy is either **generated** from that source or **compared** to it by a check that fails CI. A copy that only a comment or a Markdown table keeps in step is drift waiting to happen.

   When you add or change such a value:

   - Edit the contract first, then the code.
   - Adding a copy (a new language, writer, reader, flow type or side protocol)? Add its generator or parity check **in the same commit**.
   - Run `ci/check.sh`. It runs every gate below.

   | Source in `contracts/` | Copies | Gate |
   |---|---|---|
   | `enums/enums.yaml`, `versions/versions.yaml` | Rust `mp-contracts`; `exchange::ACTIVITIES` (`app_activities`) | `server/contracts-rs/tests/parity.rs`; `exchange::activities_match_the_contract` |
   | `packets/*.md` + `testvectors/packets/` | `core/src/packet.rs` (one codec, both nodes via JNI) | golden-vector tests in `packet.rs` |
   | `schemas/*.json` | Rust DTOs → generated `openapi.yaml` → generated Kotlin client | `ci/validate_contracts.py` (examples, plus schema vs Rust shape); `openapi_drift`; `ci/check_client_drift.sh` |
   | `parquet/*.parquet.yaml` | Rust writers; `analysis/fieldtest_analysis/schema.py` | `core/tests/parquet_contract.rs`; `analysis/tests/test_schema_contract.py` |
   | `db/schema.sql` | server migrations | migration test in `mp-server-main` |
   | all of the above, for the Logger | `Contract.java`, **generated** by `contracts/codegen/java_contract.py`: versions, enums, REST paths, row keys, mp-probe | `java_contract.py --check`; Logger JVM tests (`android/logger/src/test`) validate every body it sends against the schemas |
   | RUN_TEST flows → core | the Logger passes each flow whole (`Wire.ueConfig`) | `core/tests/test_plan_contract.rs`: the core reads every field of every example, and the examples use every flow property |
   | `packets/time_sync.md` + vectors | server-main `time_sync.rs`, Logger `ClockSampler` | golden tests on both sides |
   | `internal/mp_probe.yaml` + `testvectors/mp_probe/` | `probe/` (Rust), Logger `MpProbe`, `analysis` `bench.py` | probe tests, Logger JVM tests, `test_bench.py` |

   The Logger builds every JSON body it sends in `Wire.java` from `Contract` constants — never inline a wire string or key elsewhere.

   New fields in a JSON body need an example in `contracts/testvectors/` that uses them, because the shape check compares names and enum values, not bounds.

   **Still unchecked: fix these, don't extend them.**

   - The reach protocol (`packets/reach.md`) has no golden vectors.
   - The JSON *responses* the Logger reads (e.g. challenge, logging config) are read by hand-typed keys.
   - Value sets documented only in YAML docs (radio `network_type`, `transport`, `cell_connection_status`) are not enums yet.

## Log layout (per test)

A test aggregates **both nodes'** recordings under one `test_id`. Each node writes its per-node files (`packets.parquet`, `events.parquet`, `clock.parquet`, `summary.json`) into a `mobile/` or `server/` subfolder; the mobile-only `gps.parquet` and `radio.parquet` sit at the test root and are recorded **once per test** regardless of flow count:

```
session_<date>/test_NNN/
  header.json
  gps.parquet            # mobile only, 1/test
  radio.parquet          # mobile only, 1/test
  mobile/  { packets, events, clock .parquet + summary.json }
  server/  { packets, events, clock .parquet + summary.json }
```

See spec §12 for field-level schemas. Some `events.parquet` rows (HANDOVER, COVERAGE_GAP, BAND_CHANGE) are *derived in post-processing* from `radio.parquet`, not logged live.

## Keeping docs in sync

`AGENTS.md` and this file overlap by design. When you change architecture, build files, directory structure, or the REST/log schemas, update the spec first (it is authoritative), then reconcile both guides.
