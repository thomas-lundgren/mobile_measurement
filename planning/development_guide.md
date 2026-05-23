# Development Guide — 5G Field Test Platform

> Engineering discipline for building this system correctly across a polyglot codebase.
> Companion to: `5_g_field_test_platform_architecture_spec.md` (authoritative *what*),
> `development_plan.md` (*sequencing* and per-invariant enforcement), and `../CLAUDE.md`
> (cross-cutting invariants). This guide is the *how*: how we keep the two nodes
> symmetric and the interfaces consistent while one side is Rust and the other is Java.

---

## 0. The one principle

**Nothing that must match across components is written twice from prose.**
Every shared behavior or format has a *single source*; code is *generated* from it (not
hand-transcribed); and equality is *proven* by tests that run against **both** the Rust
node and the Java node. Everything below is an application of this principle.

---

## 1. Languages, nodes, and the symmetry contract

- **Linux server — Rust** (axum / tokio / serde / sqlx / tracing): `server-main`,
  `test-control`, `data-generator(s)`.
- **Mobile — Java** (Android foreground service) wrapping a **shared Rust core via JNI**
  (see §5): the measurement engine + logger are Rust; Java owns lifecycle, GPS, radio,
  permissions, and the control/REST client.
- **Analysis — Python.**

The server and the mobile (UE) are **symmetric peers**. Each is a generator, receiver,
loopback/echo, and logger; both participate in measurement and both produce logs. The
flow *direction* only selects which side emits payload. **Decided architecture:** the
measurement engine is *one Rust library* used by both nodes (§5), so symmetry is structural
rather than maintained by hand.

**The only allowed asymmetries** (everything else must be symmetric, and tested to be):

1. The UE always initiates the connection (NAT/firewall punch-through).
2. Only the mobile has GPS and radio/cell metrics (the mobile-only singletons).

---

## 2. Cross-cutting invariants — coding rules

Honor these in every component (full enforcement+test mechanisms in `development_plan.md` §3):

| Invariant | Coding rule |
|---|---|
| Logging-first | Enforced structurally: the engine + log writer live in the Rust core, which has *no* upload client (upload is in the Java shell). Writes go to durable segmented files that never block on network. |
| Dual-clock timestamps | The core samples a `Timestamp{mono_ns, utc_ns}` pair atomically; the logging API accepts only that pair, never a bare integer. OWD is computed only in post-processing, with uncertainty bounds. |
| Server-configured logging | The effective config is produced by exactly one function `resolve_effective_config(plan, capabilities, overrides)`; the UE only interprets it. |
| Forward-compatible logs | All log fields nullable; additive-only; readers fill missing columns `null` and ignore unknown ones. |
| UE always initiates | The data-generator has no outbound "dial UE" path; `test-control` gates generator-listening *before* the UE gets `RUN_TEST`. |
| Symmetric peers | See §3. |

---

## 3. Keeping the two nodes symmetric

Symmetry = the same flow config produces equivalent traffic and equivalent logs, and
either node can be sender / receiver / loopback. With the shared Rust core (§5) the engine
is **one implementation**, so most symmetry is by construction; the mechanisms below now
mainly prove that the *two runtimes* — the native server process and the Android-via-JNI
host — drive that one core identically, and that the allowed asymmetries hold:

- **Symmetric role abstractions.** Define the same four roles — `Generator`, `Receiver`,
  `Loopback`, `Logger` — with identical method contracts in Rust and Java. A flow is
  parameterized by `(direction, role)` the same way on both sides. Symmetry is structural,
  not accidental.
- **One normative behavioral spec** for the timing-sensitive parts: generator state
  machine, keepalive cadence (~1 Hz until first downlink), when payload starts after the
  handshake, echo/reflect semantics. Both sides implement the spec, not their own reading.
- **Deterministic, seeded generators.** Given `(seed, rate, size, count)` the core emits a
  reproducible sequence, captured once as a **golden traffic vector** (pcap/hex). It is a
  regression test for the single codec and a check that the **JNI binding** produces
  identical bytes/timing to the native call.
- **Cross-runtime interop matrix — the real proof.** Run every pairing for each traffic
  type × direction, where each side runs the *same core* in a different runtime (native
  server vs. Android-via-JNI):

  | Sender | Receiver | Echo host |
  |---|---|---|
  | Server (native) | Mobile (JNI) | — |
  | Mobile (JNI) | Server (native) | — |
  | Server client | — | Mobile (JNI) |
  | Mobile client | — | Server (native) |

  Equivalent measurements across all combinations ⇒ the JNI host drives the core just like
  the native server, and the nodes are genuinely symmetric.
- **Differential testing.** Replay one golden session config through both nodes; assert the
  resulting `summary.json` / `packets.parquet` match within tolerance.
- **Pin the allowed asymmetries.** Add explicit tests: the server never dials outbound, and
  `server/` produces no `gps.parquet` / `radio.parquet`.

---

## 4. Keeping the interfaces consistent

With the shared Rust core (§5), the data-plane wire formats and the log writer are
**single-implementation Rust**, so the remaining *cross-language* surface is just (a) the
**control plane** — the Java REST client ↔ the Rust server — and (b) the **JNI boundary**
between the Java shell and the core. One `contracts/` directory is the **source of truth**;
each contract type gets a generator and a conformance test so neither side hand-maintains
the format:

| Contract | Source of truth | Rust side | Java/Android side | Conformance test |
|---|---|---|---|---|
| Shared DTOs + enums (flow type, direction, role, states, versions) | **annotated Rust types** (`serde` + `schemars`/`utoipa`) | native | `openapi-generator` (REST) / `jsonschema2pojo` (non-REST) | enum string round-trip |
| REST API | OpenAPI 3 **derived from Rust** (`utoipa`, code-first) | `utoipa` (from axum) | `openapi-generator` (Retrofit/OkHttp) | provider + consumer (Pact) contract tests |
| Measurement packets (probe, realtime UDP) | **The Rust codec in the shared core** (§5) — single impl | `core` crate | calls core over **JNI** (no second codec) | golden vectors as codec regression + JNI round-trip |
| Control IPC (server-main ↔ test-control ↔ generator) | **local HTTP + JSON** (loopback, same-host) — JSON Schema (`contracts/internal/`, `internal_*.json`) | `axum` / `serde` (reused) | n/a (server-side only) | schema validation of the `internal_*` examples |
| Log schemas (Parquet / JSON) | one Arrow schema + JSON Schema | `arrow` / `parquet` crates (in core) | **writes via the shared core** (same `arrow`/`parquet`) | forward-compat read in Python + Rust |
| JNI boundary (engine config in; log paths / status out) | the core's exported API | `jni` exports | thin Java facade / generated bindings | config round-trip across JNI |

Rules that make it stick:

- **Generate, don't transcribe. Commit generated code, and fail CI on drift** —
  regenerating from `contracts/` must produce no diff (stops hand-editing generated files).
- **Contract tests at every boundary** (the right column above), including the
  forward-compat test: a file written by Java is read by the Rust/Python reader, and the
  reader tolerates missing/unknown columns.
- **Versioning & negotiation.** `protocol_version` agreed at registration; additive within
  a major; a **schema-diff "additive-only" gate** in CI. (`schema_version`,
  `app_version`, `logging_config_version` policy in `development_plan.md` §4.)

**Format choice matters:** the core's packet codec uses **fixed-layout** structs (not
protobuf) — protobuf's varint/tag encoding would destroy the exact byte offsets required
for RTP-like timing packets; golden vectors lock the layout. The *control* IPC uses local
HTTP+JSON (not gRPC): it is same-host, off the real-time hot path, and lockstep-released, so
the protobuf/tonic toolchain isn't worth it — reuse the axum/serde stack. See
`contracts/internal/`.

---

## 5. The shared Rust core (DECIDED)

The measurement engine is **one Rust library** (`core`) linked into both nodes: natively in
the server processes, and on Android over **JNI** (`jni` crate). One implementation cannot
drift from itself — this is what makes the nodes symmetric by construction.

**Boundary** (selected scope: *engine + logger*):

| In the Rust core | In the Java/Android shell |
|---|---|
| Packet codec (probe, realtime UDP) | Foreground-service lifecycle & threading host |
| Interval / jitter / sequence math | Permissions, networking allow-listing |
| Generator / receiver / loopback engine (opens the UDP/TCP sockets) | GPS sampling (Location API) → fed to core |
| Clock pair sampling (`mono_ns` + `utc_ns`) | Radio/cell sampling (TelephonyManager) → fed to core |
| Log writer — `arrow`/`parquet`, same code as the server | Control/REST client (register, poll, status, upload) |

So the data plane and the on-device log writer are shared Rust; Java provides only what is
intrinsically Android (lifecycle, permissions, GPS, radio) plus the control/REST client.
Server-only GPS/radio absence and UE-initiation remain the two allowed asymmetries.

**Mechanics:**

- **Keep the JNI surface small and coarse:** start/stop a flow, push GPS/radio samples, give
  the core a directory to write into, poll status. Marshal config across the boundary as one
  serialized blob — `serde` JSON (default) or CBOR, reusing the Rust-first DTOs (§10) — not
  many fine-grained calls.
- **Native sockets on Android.** The core opens BSD sockets directly; validate against
  Android networking (INTERNET permission; socket protection if a VPN/secondary network is
  present). Phase-0 spike.
- **Cross-compilation.** Build `core` for `aarch64-linux-android` / `x86_64-linux-android`
  and package the `.so` via Gradle (`cargo-ndk`); the server links it natively.
- **Crash-resilient logging.** Native Parquet writes its footer at `close()`, so the in-core
  logger **segments** — it rolls a new part file every N rows / T seconds and closes it
  promptly, so a mid-test kill loses at most the open segment (read back as a Parquet
  dataset). This is how logging-first is honored with direct Parquet.

---

## 6. Testing layers (the safety net)

| Layer | What it proves | Tooling |
|---|---|---|
| Unit / property | Local correctness; invariants (mono non-decreasing, `effective ⊆ capabilities`) | `cargo test` / JUnit; proptest / jqwik |
| Golden vectors | Single codec stays stable; JNI binding matches the native call | checked-in hex/pcap fixtures |
| Contract tests | REST + IPC payloads match the contract | OpenAPI provider/consumer, Pact |
| Interop matrix | Both runtimes (native + JNI) drive the one core identically (§3) | end-to-end harness |
| Differential / golden session | Same config ⇒ equivalent logs/summaries | replay harness |
| Forward-compat round-trip | Old fixtures readable by new readers; unknown columns tolerated | Python + Rust readers |
| Chaos | Logging-first holds under network loss | fault-injection integration test |

---

## 7. CI gate & definition of done

A **single CI job** is what actually *ensures* symmetry and consistency on every change:

> build Rust + Android → regenerate from `contracts/` (assert **no diff**) → run golden
> vectors in both languages → run log round-trips → run the **interop matrix** → run the
> **additive-only** schema check → run the logging-first chaos test.

**Definition of done** for any change that touches a contract or a node role:

- [ ] `contracts/` updated first; generated code regenerated and committed.
- [ ] Golden vectors / fixtures added or updated.
- [ ] Conformance + contract tests pass in **both** languages.
- [ ] Interop matrix green for affected traffic types/directions.
- [ ] Version fields bumped per policy if the change is breaking.
- [ ] Spec updated (authoritative), then `AGENTS.md` / `CLAUDE.md` reconciled.

---

## 8. Proposed `contracts/` layout

```text
contracts/
  openapi/            # REST API (device-facing + controller-facing; incl. /devices/enroll, /auth/challenge, /auth/verify, /auth/login)
  internal/           # server-side control IPC (HTTP+JSON) + generator→logger sink spec
  packets/            # layout specs + testvectors/ (golden hex/pcap)
  schemas/            # JSON Schema (header.json, summary.json, capability, logging_config)
  parquet/            # Arrow schema definitions per log file
  enums/              # flow type, direction, role, states, event types
  auth/               # AUTH_SIGNING_ALG, AUTH_PUBKEY_FORMAT, AUTH_SIG_FORMAT, AUTH_DOMAIN
  versions/           # protocol_version, schema_version, ... single source
  testvectors/        # shared golden inputs/outputs used by both languages
                      #   incl. auth_signing/ (priv_key_pem, pub_key_spki_b64,
                      #   message_hex, signature_der_b64) — round-trip on every impl
```

**Auth contract (cross-platform).** Device auth is challenge/response over an Android
Keystore / iOS Secure Enclave P-256 keypair; the contract pins
`ECDSA_P256_SHA256` + `SPKI_DER` pubkeys + `ASN1_DER` signatures so the same server path
verifies signatures from any platform. The signing golden vector in
`contracts/testvectors/auth_signing/` is the gate against the DER-vs-IEEE-P1363 interop
bug — every Rust verifier, Android signer, and (future) iOS signer must round-trip it in
CI. Auth model rationale: `development_plan.md` §7.

---

## 9. Android-specific caveats

- **Parquet on Android — resolved.** The log writer lives in the Rust core (`arrow`/
  `parquet`), so there is *no* Java Parquet dependency (`parquet-mr` / `parquet-floor` /
  Arrow-Java all dropped). The mobile writes the identical Parquet the server does.
- **JNI cross-compilation is committed.** Build `core` for the Android ABIs and package the
  `.so` (`cargo-ndk`) in Gradle.
- **Native sockets** opened by the core must be validated on Android (permissions, VPN /
  multi-network socket protection) — Phase-0 spike (§5).
- The logger is a **foreground service**; the in-core writer **segments** files so the
  logging-first path survives background kills and network loss (§5).

---

## 10. Decisions

**Resolved:**

- **Shared Rust core via JNI** — *engine + logger* boundary (§5). Nodes symmetric by
  construction; mobile writes Parquet through the same `arrow`/`parquet` as the server.
- **Android log writer** — superseded: the writer is in the Rust core, so the
  `parquet-floor` / Arrow-IPC / `parquet-mr` choice no longer applies.

- **DTO source — Rust-types-first.** Control-plane DTOs/enums are authored as annotated
  Rust types (`serde` + `schemars`/`utoipa`); Java is generated from the derived OpenAPI
  (`openapi-generator`) and from the emitted JSON Schema for non-REST types
  (`jsonschema2pojo`); Python from the JSON Schema (`datamodel-code-generator`). Rust is
  authoritative because it is both the server and the core. **For consistency this fixes
  REST as code-first (`utoipa`)**, not spec-first — one authoring model. For independent /
  contract review, *extract* the generated OpenAPI + JSON Schema and annotate those
  artifacts (don't fork the source). The emitted JSON Schema is also the additive-only
  diff-gate artifact.

- **JNI config marshalling — serialized blob.** The Java shell hands the core a single
  serialized config blob (`serde`: JSON by default — debuggable, infrequent, small; CBOR if
  a payload turns large/hot — same DTOs either way), not fine-grained typed bindings. This
  reuses the Rust-first DTOs directly (serialize the flow/test-config type, deserialize in
  the core) and keeps the JNI surface tiny and stable.

**Still open:**

- None. Remaining work is execution — see `development_plan.md` (next concrete step: the
  Phase-0 JNI spike — native sockets + `cargo-ndk` + segmented Parquet surviving a mid-test
  kill on a Pixel 9 Pro).

---

## Keeping this guide current

When the toolchain, `contracts/` layout, or CI gate changes, update this guide. When the
*architecture* changes, update the spec first, then reconcile `AGENTS.md` and `CLAUDE.md`,
then revisit this guide.
