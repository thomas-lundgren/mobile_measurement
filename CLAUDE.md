# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Current state: planning phase

There is **no source code, build system, or tests yet**. The repository holds these documents:

- `AGENTS.md` — agent-oriented guide (planned stack, components, state machines, REST API).
- `planning/5_g_field_test_platform_architecture_spec.md` — the authoritative architecture & functional spec. Read this for any non-trivial detail (packet formats, field-level log schemas, OWD math, endpoint payloads).
- `planning/development_plan.md` — build sequencing, per-invariant enforcement+test mechanisms, and the contract-first strategy for cross-component interface consistency.
- `planning/development_guide.md` — engineering discipline for the polyglot build: how to keep the Rust server and Java mobile node *symmetric* and their *interfaces consistent* (single-source contracts, codegen, golden vectors, interop matrix, CI gate).

Because nothing is built, there are no build/lint/test commands. **When you add the first source files or build config, populate this file (and `AGENTS.md`) with the concrete commands**, e.g. Rust workspace (`cargo build`/`cargo test`/`cargo test <name>`), Android Gradle, and the Python analysis toolchain.

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
