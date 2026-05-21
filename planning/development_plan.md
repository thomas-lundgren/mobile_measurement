# 5G Field Test Platform — High-Level Development Plan

> Companion to `5_g_field_test_platform_architecture_spec.md` (authoritative spec) and `../AGENTS.md`.
> This document covers **sequencing**, **how the cross-cutting invariants are enforced**, and **how interface consistency is kept across the polyglot codebase** (Rust server / Kotlin Android / Python analysis).

---

## 1. Guiding strategy

The platform is three independently-built components in three languages that must agree on the same wire formats, REST API, log schemas, config schema, and command/state vocabulary. The single largest risk is **contract drift** between them. Two principles drive the plan:

1. **Contract-first.** All inter-component contracts live in one versioned `contracts/` directory and are the source of truth. Code is generated from them or conformance-tested against them — never the reverse.
2. **Lock contracts before parallel work.** Phase 0 produces the contracts; afterwards Android, server, and analysis can proceed in parallel against fixtures, because every boundary is pinned.

---

## 2. The contracts (single source of truth)

`contracts/` — versioned, owns every cross-component boundary:

| Contract | Format | Producers / consumers | Generation / verification |
|---|---|---|---|
| **REST API** (register, command poll, status, summary, upload) | OpenAPI 3 | server-main ⇄ Logger, server-main ⇄ Controller | Generate Rust types + Kotlin client; provider verification test against the spec |
| **Binary wire packets** (`ProbePacket`, `RealtimePacket`) | One annotated layout table (offsets/widths/endianness) **+ golden test vectors** (hex blob → expected decode) in `contracts/testvectors/` | data-generator ⇄ Logger traffic engine | Each language has an encode/decode conformance test over the golden vectors |
| **Log schemas** (`header.json`, `summary.json`, `*.parquet`) | JSON Schema + a single Arrow/Parquet schema definition | Logger writes → server stores → Python reads | Fixture round-trip test; **additive-only schema-diff gate** in CI |
| **Capability + LoggingConfig** | JSON Schema | Logger reports ⇄ server resolves | `resolve_effective_config()` property tests (see §3.3) |
| **Commands + state-machine names** (`RUN_TEST`, `SET_LOGGING_CONFIG`, state enums) | One enum source (YAML) | all components | Generate constants per language; string round-trip test |
| **Version constants** (`protocol_version`, `schema_version`, `logging_config_version`) | One file | all | Negotiated at registration; see §4 |

**Why golden vectors instead of protobuf for packets:** the packets are fixed-width network structs, not a self-describing format. A hand-maintained layout table plus checked-in hex vectors that every language must encode/decode exactly is cheaper than a codegen toolchain and catches the real failure modes — endianness, field width, and offset drift.

---

## 3. Handling the cross-cutting invariants

Each invariant gets a **single enforcement point** (so it can't be violated piecemeal) and a **conformance test** (so regressions are caught). Summary table, then notes.

| Invariant | Enforcement point | Conformance test |
|---|---|---|
| 1. Logging-first | Module boundary: test-runner has **no reference** to the upload client; logging writes go through a durable local queue that never blocks on network | Chaos test: cut server connectivity mid-test → assert measurement continues, all rows logged, test reaches COMPLETED/PARTIAL, upload retries later |
| 2. Dual-clock timestamps | A `Timestamp { mono_ns, utc_ns }` type sampled atomically; the logging API accepts **only** this type, never a bare `u64` | Every logged row has both fields; `mono_ns` non-decreasing; OWD uncertainty-propagation property test |
| 3. Server-configured logging | One function `resolve_effective_config(plan, capabilities, overrides)` on the server; UE only interprets | Property test: `effective ⊆ capabilities` for all inputs; drift test: stale `logging_config_version` triggers `SET_LOGGING_CONFIG` |
| 4. Forward-compatible logs | Schema additive-only; tolerant reader fills missing columns `null`, ignores unknown columns | Old fixture readable by new reader; reader tolerates extra unknown columns; CI additive-only gate |
| 5. UE always initiates | data-generator has **no outbound dial-UE path**; test-control gates `WAITING_FOR_GENERATOR → (listening) → WAITING_FOR_CLIENT` | NAT punch-through interop test; FromServer-UDP keepalive-until-first-DL test |
| 6. Symmetric endpoints | One traffic-engine core per platform, built to the shared packet contract; direction is a parameter (probe is the echo special case) | Interop matrix: Rust generator ⇄ Kotlin receiver, both directions + echo |

Notes on the subtle ones:

- **(1) Logging-first** is an *architectural* guarantee, not a runtime check — make it impossible to express. The active-test code path must not be able to import the upload client; uploads read only sealed files. Verify the dependency direction in CI.
- **(2)** UTC is never trusted for ordering; OWD is computed **only in post-processing** with uncertainty bounds derived from `clock.parquet` (mobile) and chrony/Netnod stats (server). Record server clock uncertainty too, not just the mobile's.
- **(3)** The effective config is the *intersection* of test-plan profile and device-reported capabilities, with per-device overrides applied last. Resolution lives in exactly one place so the rule can't fork.
- **(5)** During tests the UE is forced to **RRC_CONNECTED active** (DRX out of scope). The ordering guarantee — generator listening *before* the UE gets `RUN_TEST` — is the test-control state machine's job.

---

## 4. Versioning & compatibility

Three independent version numbers, all negotiated at device registration:

- `protocol_version` — REST + binary wire formats.
- `schema_version` — log files; **bumped only on breaking change** (additive changes don't bump it).
- `logging_config_version` — carried in every status upload so the server can detect drift and re-push config.

Policy: additive within a major version; a breaking change bumps the major and opens a dual-support window. The server stores **all** fields present in an upload, even beyond the baseline schema (forward compatibility for more-capable devices).

---

## 5. Phased build plan

Sequencing front-loads contracts and proves the control plane on localhost before any Android dependency.

### Phase 0 — Foundations & contracts

Two workstreams run here: the **JNI viability spike** (Step 0, below — it gates the
architecture) and authoring `contracts/`. They may overlap, but the spike must pass before
Phase 2 (engine) work begins.

**Contracts & foundations:**
- Confirm stack: Rust server (axum/tokio/serde/sqlx/tracing); **Android Java + shared Rust core via JNI** (see `development_guide.md` §5); Python analysis.
- Monorepo layout (Cargo workspace, Gradle, Python package), CI.
- Author all of `contracts/` (§2): OpenAPI (code-first via `utoipa`), packet layout + golden vectors, log JSON Schema + Arrow schema, capability/config schema, enums, version constants.
- Define shared primitives: `Timestamp` pair, IDs (node/device/session/test), version constants.
- **Exit:** contracts generate/compile, golden vectors exist, CI green.

#### Step 0 — JNI viability spike (gates the shared-core architecture)

**What it is.** A time-boxed proof-of-concept that the shared Rust core actually runs over
JNI — including native sockets and on-device Parquet — **before** any engine code is written.
It runs in **two stages**: most of the *logic* is proven cheaply on the server / a desktop
JVM (**Stage A**), and only the irreducibly Android-specific risks are confirmed on a real
Pixel 9 Pro (**Stage B**). Stage A is largely **reusable** — the native core + its harness
are essentially the Phase-1 server side; only the Stage-B Android wrapper is throwaway
(`spikes/`).

**Why it exists.** The architecture (`development_guide.md` §5) rests on assumptions that
can't be settled by reasoning. Stage A catches the *code* bugs; Stage B confirms the Android
environment (cross-compile, cellular, foreground service) that Stage A cannot reproduce. Each
failure maps to a pre-identified fallback.

**Shared slice — one UDP probe flow.** The core opens UDP sockets, samples the dual clock,
encodes/decodes the probe packet, and writes a **segmented** `packets.parquet`; a thin host
(`init` / `start_flow` / `poll_status` / `stop` over a serde JSON config blob) drives it. In
Stage A the host is a desktop JVM and the peer is a server process running the same core; in
Stage B the host is the Android foreground service and the peer is the server.

##### Stage A — server + desktop JVM (Linux x86_64; CI-able; mostly reusable)

| Check | Pass criterion |
|---|---|
| JNI binding | a desktop JVM `System.loadLibrary` loads the Rust `cdylib`; a JNI call returns a value; the serde config blob round-trips Java→core→Java |
| Socket code | probe echo completes on localhost/LAN for ~100% of packets |
| Resilience logic | injected loss/blackout (`netem` / iptables) mid-run → core keeps logging and recovers (logging-first holds) |
| Dual clock | `mono_ns` monotonic non-decreasing; `(mono,utc)` sampled in Rust agrees with a Java-sampled pair within a few ms |
| Parquet writer | valid **segmented** `packets.parquet`; reads back in **pyarrow and arrow-rs** with the expected schema + row count |
| Crash strategy | `SIGKILL` the process mid-write → every closed segment is readable; loss ≤ one open segment |
| Footprint (proxy) | record the Linux `.so` size as a rough proxy for the APK delta |

**Exit A:** all green in CI. (Covers the code/logic half of original criteria 1, 2, 3, 6, 8
and all of 4, 5, 7.)

##### Stage B — Pixel 9 Pro (irreducible; physical device)

| Check | Pass criterion |
|---|---|
| Cross-compile + ART load | `cargo-ndk` builds an `arm64-v8a` `.so`; APK installs; loads + a JNI call returns under ART on the Pixel 9 Pro |
| Sockets over cellular | UDP send/recv from native code to the server over **mobile data** + NAT punch-through; echo ≥99% (excluding genuine loss) |
| Real mobility | a Wi-Fi↔cellular switch or cell handover mid-run does not crash; logging continues |
| Android clock | the dual clock behaves across suspend/doze |
| Android FS + on-target crate | the cross-compiled `arrow`/`parquet` writes valid Parquet to app-private storage |
| Foreground-service kill | OS-killing the foreground service mid-test → closed segments survive (loss ≤ one segment) |
| Real APK size | `.so` APK delta measured (arm64; arm64+x86_64), within budget (target e.g. ≤ ~12 MB arm64 — confirm) |
| FG longevity | a continuous ≥5-min run completes without an OS kill under normal conditions |

An Android **emulator** covers cross-compile + ART load + FS, but has no modem/doze — so
cellular, mobility, FG-kill, and longevity require the physical Pixel. (Covers the
environment half of original criteria 1, 2, 3, 5, 6 and all of 9 + the real measurement for 8.)

**Exit B:** all green on the device → go.

**Fallbacks if a criterion fails** (redirects the architecture, not just a fail):
- Cellular sockets (Stage B) unworkable → move the socket layer to Java; narrow the core to codec + timing + logger.
- Parquet (Stage A or B) unworkable on the Android target → re-open the writer decision (Arrow-IPC-then-convert, or `parquet-floor`).
- Cross-compile / JNI (Stage B) unworkable → re-open the shared-core decision (two engine impls kept in step by golden vectors).

**Deliverables:** the Stage-A core + harness (kept — feeds Phase 1); the Stage-B Android
wrapper (`spikes/`, throwaway); a one-page findings note with each check's result +
measurements (RTT, clock delta, APK delta, segments recovered after kill); a go/no-go with
any boundary adjustment.

**Gate:** Stage A green before Stage B; Stage B green before Phase 2 (engine). Stage A may
overlap contracts authoring.

### Phase 1 — Server skeleton + protocol loopback
- `server-main`: REST endpoints, device registry, command queue, persistence (sqlx).
- `test-control`: per-device state machine, port allocation, generator lifecycle with READY gating, internal control API.
- `data-generator`: UDP probe (echo) first, then realtime UDP, then bulk TCP — all directions, dual timestamps, generator state machine.
- Synthetic test client (Rust or Python) that registers, polls, runs a probe against the local generator.
- **Exit:** full control plane + probe end-to-end on localhost, no Android needed.

### Phase 2 — Android Logger (MVP)
- Foreground service, registration with capabilities, command poller, logger state machine.
- Logging-first local store (durable, decoupled); upload manager (policies) decoupled.
- Traffic engine: integrate the **shared Rust core via JNI** (not a Java reimplementation) — the core opens sockets, runs probe / realtime UDP / bulk TCP (UE-initiated, all directions), and writes logs; the Java shell drives it and owns the control/REST client.
- Radio (Telephony) + GPS (Location) sampled in the Java shell and fed into the core; clock pair + calibration in the core.
- Interop-tested against the Phase-1 server.
- **Exit:** a Pixel 9 Pro runs a full test, logs locally, uploads.

### Phase 3 — Android Controller (MVP)
- Auth (stub first), generated server API client, device list, start/stop test, upload control, status dashboard.
- **Exit:** operator drives a test from the controller.

### Phase 4 — Analysis toolchain (Python)
- Tolerant loaders (forward-compatible); OWD-with-uncertainty, RTT/jitter/loss; route/RTT/radio-correlation/packet-timing plots; handover + derived events from PCI/NCI changes.
- **Exit:** standard outputs produced from a real uploaded session.

### Phase 5 — Hardening & field readiness
- Failure-handling (network loss, partial results, reconnect), multi-device fleet, dynamic `SET_LOGGING_CONFIG`, clock-uncertainty validation, battery/storage, chaos tests for logging-first.
- **Exit:** a real multi-device field run.

**Parallelism:** after Phase 0–1, Phases 2 (Android) and 4 (Analysis) run in parallel — both are pinned to contracts and fixtures.

---

## 6. CI gates (continuous enforcement)

- Generated code in sync with `contracts/` (regeneration produces no diff).
- Schema additive-only diff check.
- Golden-vector packet conformance in every language.
- Interop matrix (native server ⇄ Android-via-JNI traffic, both directions + echo).
- Log round-trip + old-fixture-readable-by-new-reader.
- Logging-first dependency-direction check (test path cannot reach upload client).

---

## 7. Decisions

**Resolved** (see `development_guide.md` §10): Android = **Java + shared Rust core via JNI**;
Parquet writer = **in the Rust core**; REST/DTO codegen = **Rust-first** (`utoipa` / `schemars`);
JNI config = **serde blob**.

**Repo structure — monorepo.** One repository for all components. Decisive here because:
(a) the shared Rust core is linked into *both* the server (natively) and the Android app
(`cargo-ndk` → JNI), so they must build from the **same source at the same commit** — no
artifact publishing, no version skew on the one library whose value is being a single
implementation; (b) the code-first contracts drift gate ("regenerate → assert no diff") plus
the golden-vector and interop-matrix checks span languages and run as **one pipeline**;
(c) a packet/DTO change touches Rust + generated Java + Python + golden vectors as **one
atomic PR**. Split repos only pay off for independent release cadence, separate ownership, or
open-sourcing a component — none apply yet, and a monorepo can be split later (easier than
merging).

Layout:

```text
/
  Cargo.toml          # Rust workspace
  core/               # shared engine+logger lib (native + cargo-ndk Android target)
  server/             # server-main, test-control, data-generator (workspace crates)
  android/            # Gradle: logger/ + controller/ (consume core's .so via JNI)
  analysis/           # Python package
  contracts/          # single source of truth (see development_guide.md §8)
  spikes/             # throwaway experiments (e.g. the JNI viability spike)
  ci/                 # pipeline definitions
```

Guardrails: **path-based CI triggers** (don't rebuild Android unless `core/`, `android/`, or
`contracts/` changed); **independent release tagging** per component (a monorepo co-locates
*source*, it does not force lockstep *releases* — important since Android ships through Play
review on a slower cadence than the server); **Git LFS** for large golden vectors / pcaps.

**Auth model — bearer tokens over TLS, two principal types, one validation pipeline.**
The Controller is a *human operator*; the Logger is an *autonomous device* — different auth
needs, served by one middleware on `server-main`. (Resolves review items 3.8 "no auth" and
M5 "self-asserted `device_id`".)

- **Transport:** **TLS mandatory** on all external endpoints (bearer tokens require it).
- **Operator (Controller):** username/password login → `server-main` issues a short-lived
  **JWT** carrying a role claim (`viewer` / `operator` / `admin`; MVP uses `operator`). The
  Controller sends `Authorization: Bearer <jwt>`; middleware validates signature + expiry per
  request. Slots into OIDC/SSO later by swapping the issuer — the validation path is unchanged.
- **Device (Logger/UE):** **enrollment + challenge/response, not a self-asserted id.** On
  first launch the device generates a non-exportable **P-256 keypair** in the platform secure
  store (Android Keystore; iOS Secure Enclave) and exchanges a one-time **enrollment token**
  for a server-assigned `device_id`/`node_id`, registering its **SPKI public key**. There is
  no long-lived bearer — the keypair *is* the credential. Each session: `POST /auth/challenge`
  returns a random nonce; the device signs a domain-separated blob
  (`"mp-auth-v1" || server_id || challenge_id || nonce`) with `ECDSA_P256_SHA256` /
  `ASN.1 DER`; `POST /auth/verify` returns a short-lived opaque session token used on all
  later calls. Server derives identity **from the verified signature, never from a
  client-claimed `device_id`** (fixes M5). Revocation = delete the pubkey row.
- **Cross-platform auth.** The challenge scheme is deliberately platform-neutral so iOS (and
  future clients) work with the same server code path. The contract pins **algorithm
  (`ECDSA_P256_SHA256`), pubkey encoding (`SPKI_DER`, base64), and signature encoding
  (`ASN1_DER`)** — all three natively supported by Android Keystore and iOS Secure Enclave;
  only the per-platform key-gen/sign glue differs. Phase 0 ships a cross-platform signing
  **golden vector** (`priv_key_pem, pub_key_spki_b64, message_hex, signature_der_b64`) in
  `contracts/testvectors/` that every implementation must round-trip — this is what prevents
  the DER-vs-IEEE-P1363 class of interop bug. Note: iOS is **not** in MVP scope (Pixel 9 Pro
  is the reference per `CLAUDE.md`); designing the auth contract platform-neutrally is cheap
  now and avoids a breaking change later, but iOS *implementation* stays out of Phase 2.
- **Authorization / scoping:** two distinct token audiences — operator tokens for the
  controller endpoints, device tokens scoped to device endpoints and to their own
  `device_id`. A device token can't call controller APIs and vice versa.
- **Secrets:** JWT signing key + device enrollment secrets live in server config / a secret
  store, with rotation supported.
- **Contract impact (Phase 0):** add `POST /auth/login` (operator), `POST /devices/enroll`,
  `POST /auth/challenge`, `POST /auth/verify` to the OpenAPI; apply the `Authorization`
  scheme to every endpoint; add the cross-platform ECDSA signing golden vector to
  `contracts/testvectors/`; add versioned constants `AUTH_SIGNING_ALG = "ECDSA_P256_SHA256"`,
  `AUTH_PUBKEY_FORMAT = "SPKI_DER"`, `AUTH_SIG_FORMAT = "ASN1_DER"`, `AUTH_DOMAIN = "mp-auth-v1"`.

**Still open:** none.
