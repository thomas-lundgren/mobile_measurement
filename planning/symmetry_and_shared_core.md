# Symmetry by construction: contracts-first + shared Rust core

Two architectural decisions sit underneath nearly every line of code in this
repo. They look like engineering hygiene at first glance and turn out to be
the load-bearing structure of the polyglot build. This note explains why they
exist, what they buy, and what they constrain. For the *how* (concrete
codegen, CI gates, layout), see `development_guide.md`.

---

## The problem they solve

The platform is intrinsically polyglot: a Linux server (Rust) and an Android
node (Java + native). Both sides must agree on:

- **REST and command payloads** (operator API, device commands, internal RPCs)
- **Wire packet formats** (UDP probe, realtime UDP, bulk TCP framing, DSCP probe)
- **On-disk log formats** (Parquet column schemas for packets, radio, gps, clock)
- **Enums** (flow types, directions, generator states, command names, capability
  fields)
- **Semantic invariants** (UE always initiates the connection; dual-clock
  timestamps; logging-first)

In a naive split each side defines its own shape, mostly correct, slightly
divergent — and the divergence leaks into production as silent data loss,
mis-decoded packets, or analysis pipelines that disagree with the device.
Cross-language polyglot projects classically rot at this seam.

We made two decisions to remove the seam, not patrol it.

---

## Idea 1 — Contracts as the single source of truth

There is exactly one place that defines every cross-component interface:
`contracts/`. Nothing else is allowed to redefine these shapes; every
implementation either *generates from* the contract or *parity-tests against*
it.

The repo layout makes the rule physical:

```
contracts/
  versions/           # PROTOCOL_VERSION, AUTH_*, framing constants
  enums/              # one enums.yaml — the only definition of every wire enum
  schemas/            # JSON Schema for every REST/command DTO
  packets/            # binary wire formats (Probe, Realtime, DscpProbe)
  parquet/            # column-by-column .parquet.yaml schemas
  testvectors/        # hex blobs + JSON examples = the cross-language tie
  db/schema.sql       # the single SQL schema the server migration mirrors
  openapi/openapi.yaml# generated from the Rust server; parity-tested
  internal/           # server↔generator HTTP contracts
  README.md           # status table per contract
```

### Why a *contracts directory* and not just "one of the languages owns it"

Picking the Rust types as canonical would force Java to chase Rust changes.
Picking JSON Schema as canonical works for REST but says nothing about
binary packet layouts or Parquet columns. So we picked **the artifact, not the
language**: human-readable, language-neutral files that each language consumes
in the most idiomatic way it can.

That gives one important property: a contract change is a single PR that
*has to* touch the contracts/ directory. Reviewers know where to look. The CI
parity gate fails any PR that updates an implementation without updating
contracts (or vice versa).

### Three enforcement mechanisms — pick the cheapest that actually works

For each contract we use the lightest-weight check that closes the seam:

1. **Codegen** for things where it's straightforward — REST OpenAPI is
   generated from the Rust server's `utoipa` annotations and the resulting
   `openapi.yaml` is diffed against the committed file (the `openapi_drift`
   test). Drift is a build failure, not a runtime surprise.

2. **Parity tests** when codegen would be heavier than the parity check —
   `parity.rs` deserializes the committed YAML enums + JSON examples and
   asserts they round-trip through the Rust types. The same fixture set is
   read by the Java side. Same input + same assertions = symmetric behaviour.

3. **Golden vectors** for binary formats — for every packet type we commit a
   handful of `{description, hex_bytes, decoded_struct}` rows. Each
   implementation has a one-line test: "decode this hex, expect that struct;
   encode that struct, expect this hex". If Rust and Java both pass against
   the same vector file, they're bit-compatible on the wire by construction.
   No "we tested it on a localhost loopback and it worked" — that's a
   smoke test, not a guarantee.

### What it costs

Real friction. Adding a new wire field is "edit five contract files, regen,
fix the four implementations the parity tests now reject, write a golden
vector." This *feels* slow. It is in fact *exactly the work* needed to keep
two implementations honest — we've just chosen to pay it all at PR time
instead of debugging a desync six months later from a Parquet file written
by a fleet of phones with no operator on call.

The discipline is documented in `development_guide.md`. The rationale is:
prefer **explicit cross-language gates** over **implicit cross-language
testing** (i.e., running both sides on localhost and hoping). Localhost
interop tests still exist — they just sit on top of the parity gates, not
in place of them.

---

## Idea 2 — Shared Rust core (`core/`) for everything that's not a UI

The naive Android architecture would write the traffic engine, the log
writer, the packet codecs, the timestamp logic, and the auth crypto in
Java/Kotlin — mirroring the Rust server. That's where the *behavioural*
desync risk lives: not in the wire format (golden vectors catch that), but
in how each side *interprets* a measurement once it has the bytes.

Concrete examples:

- Server's probe-echo runs hot-path "receive → timestamp → push → echo →
  timestamp → push". If the Java mobile version timestamps in the wrong
  order, OWD is wrong and no parity test catches it — the bytes are fine,
  the *semantics* drift.
- DSCP probe: if Java uses `ChannelOption` with the wrong shift, the IP TOS
  byte is wrong by a factor of four. Bytes still parse. Numbers in Parquet
  are just *wrong*.
- Parquet column types: if Java writes `dscp_sent` as int32 and Rust as
  uint8, analysis pipelines silently coerce one of them. Now your operator
  sees `-1` instead of `255` for "unknown".

So the rule is: **anything that processes a measurement is shared Rust code
in `core/`, exposed to Java through a thin JNI shell.**

```
core/                       # platform-neutral Rust, no Android, no axum
  src/
    packet.rs               # ProbePacket, RealtimePacket, DscpProbePacket
    timestamp.rs            # dual-clock Timestamp::now()
    log_writer.rs           # Parquet writer + SPSC handoff
    aux_log.rs              # gps/radio/clock Parquet
    auth.rs                 # ECDSA P-256 sign/verify (shared with server)
    ue.rs                   # UE traffic engine (one thread per flow)
```

The same `core::ue::UeEngine` that runs in the Android Logger process runs
in the server's `ue_interop` integration test — that test isn't a mock, it's
the real engine driving the real `data-generator` over localhost. If a flow
type works in `ue_interop`, it works on the phone.

The Android Logger is then a small Java shell:

- foreground service lifecycle (Android-specific — Java)
- permission prompts, notification UI (Android-specific — Java)
- JNI seam that hands the `RunTestPayload` JSON to `core::ue::UeEngine::start`
  (Rust — `RustCore` JNI module)
- Android-specific sensors not in core: radio telemetry from `TelephonyManager`,
  GPS from `LocationManager` — these write through `core::aux_log` so the
  Parquet format is identical to anything the server might produce

### Server-side symmetry

The same core is `core/` is a path dependency of `mp-server-main` and
`mp-data-generator`. The server's probe-echo loop calls `core::packet::ProbePacket::decode`
and `core::log_writer::PacketRow::probe(...)` — the same functions the UE
calls when it originates the packet. There is one definition of what a probe
packet *is* and one definition of what a packets.parquet row *looks like*.

### Why Rust specifically

Three reasons stacked:

1. **No GC on the hot path.** The realtime UDP loop has a budget of ~10 ms
   per packet at 100 pps. The Android Runtime ART can pause for tens of
   ms on a young-gen collection. A Rust thread with a wait-free SPSC ring
   handoff to a writer thread is deterministic enough that we can
   distinguish radio jitter from measurement jitter — the whole point of
   the platform.

2. **Cross-compilation is the cheapest polyglot story available.**
   `cargo build --target aarch64-linux-android` produces a `.so` that the
   Logger app links via JNI. Same source code, same tests, no second
   implementation. The cost is wrangling NDK toolchains once.

3. **Server reuse.** Auth, timestamp logic, packet codecs, log writer —
   all of these need to exist on the server anyway. By writing them in
   `core/`, the server gets them too. The Phase-2 integration test runs
   the actual Rust core against the actual Rust data-generator on the
   actual REST API — it's an end-to-end proof that the same engine that
   will run on the phone interoperates with production server code.

### What it costs

- Android-side debugging crosses a JNI boundary, which is harder to
  step through than pure-Java. Tooling helps (logcat from Rust panics,
  `ndk-stack` for symbolicated crashes), but it's a real friction.
- Anything that genuinely needs Android APIs (Telephony, Location, foreground
  service) has to live in Java and call across the JNI boundary in the
  right direction. The seam needs to be small and JSON-shaped, which means
  serialization overhead — fine for control plane, would be a problem on
  the data plane (so the data plane doesn't cross the seam).
- `core/` cannot depend on axum/tokio/sqlx/jni — anything that pulls in a
  platform. This means the server's HTTP handlers can't live there; only
  the *engine* below them does.

---

## How the two ideas reinforce each other

The contracts directory defines *shapes*. The shared Rust core defines
*behaviour*. Either one alone leaves a class of bugs uncovered:

- Contracts without shared core: bytes match but two implementations
  *interpret* the same bytes differently (timestamp order, DSCP shift,
  Parquet null semantics).
- Shared core without contracts: shapes drift because two consumers
  (operator UI, analysis pipelines) consume them and there's no canonical
  definition to point either at.

Together they collapse the surface area where Rust-side and Java-side can
disagree to a small, named seam — the JNI control plane — which is itself
governed by a contract (`internal/control_api.md`).

---

## Practical consequences for changes

When adding or modifying *anything* that crosses the seam, the order is:

1. **Update contracts first.** A new wire field, a new enum value, a new
   REST payload — change the YAML/JSON Schema/SQL/Parquet doc that defines
   it. If a golden vector exists for the affected packet/payload, update it
   too.
2. **Update `core/`** if behaviour changes. The shared Rust code is the
   only place that decodes/encodes/logs/timestamps the new thing.
3. **Update the server** (`server-main`, `data-generator`, `test-control`)
   to use the new core APIs.
4. **Update the Java shell** if the JNI seam shape changed (rare — most
   changes don't reach the seam).
5. **Run the parity gates** — `parity.rs`, `openapi_drift`, the
   `migration_matches_contract_schema` test, golden-vector tests in
   `packet.rs`. If any fail, the contracts and an implementation
   disagree — fix until they agree.
6. **Run the interop test** (`ue_interop`) — the host-mode proof that the
   shared core drives the real server through the new shape end-to-end.

This recent dscp_probe work is a textbook example: one PR added the wire
packet definition (`contracts/packets/dscp_probe.md`), the enum entry
(`enums.yaml`), the JSON Schema (`test_plan.schema.json`), Parquet columns
(`packets.parquet.yaml`), golden vectors, the SQL CHECK constraint, the
Rust packet codec, the Parquet writer, the UE engine flow, the server
data-generator echo, the test-control port binding, and the interop test.
Every layer was touched in the same change because they all reference one
contract — and any layer left behind would have been caught by a parity
gate before merge.

---

## When *not* to use these patterns

- **Single-language services.** If we ever build a second pure-Rust
  service that doesn't talk to Android, internal types are fine; no need
  to round-trip through contracts/.
- **UI state.** The operator Controller app's local state, the Logger
  app's notification text — these don't cross the seam. Don't pay the
  contracts tax for them.
- **Experiments and spikes.** `spikes/` exists explicitly to bypass these
  gates. Promotion from spike to production code is when contracts start
  applying.

The rule is "every cross-language seam is governed by a contract." The
corollary is "anything *not* on a seam doesn't need one."
