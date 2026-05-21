# Review Remediation Plan — 5G Field Test Platform

> Date: 2026-05-20
> Consolidates: `design_review.md` (original spec review) + the meta-review of that review.
> Companion to `5_g_field_test_platform_architecture_spec.md` (authoritative spec) and `development_plan.md`.
> Purpose: one tracked list of every finding, a corrected priority, the workstream that owns it, and how it gets resolved.

---

## How to read this

Most findings are **not** independent spec holes — roughly half collapse into one root cause: *the cross-component contracts are not written yet.* They are therefore grouped into four **workstreams**, each mapped to a phase of `development_plan.md`:

- **A — Contracts** (`contracts/`, dev-plan Phase 0): every undefined schema/payload/API.
- **B — Measurement semantics** (spec edits affecting data correctness): wire/log details that change what the numbers mean.
- **C — Consistency & hygiene**: cross-document drift, terminology, naming, and fixing the review's own errors.
- **D — Security**: trust and authentication.

**Priority legend:** P0 Blocker · P1 High · P2 Medium · P3 Polish.
**Δ column:** change from the priority assigned in `design_review.md` (— = unchanged, NEW = added by meta-review).

---

## Corrections to the original review (do these first — they cost nothing and restore trust in the doc)

| ID | Issue in the review | Correction |
|---|---|---|
| C1 | **2.3** justifies "two flows" by claiming *"the earlier text said 'separate flow_id'"* — that phrase **does not exist** in the spec (verified by grep). | Keep the recommendation (model bidirectional as two unidirectional flows); delete the false citation. |
| C2 | **2.6** says SIGNAL_LOSS detection is *"never defined."* §12.7 actually gives a partial criterion: *"no serving cell for >N seconds."* | Re-scope to "underspecified": `N` and the detection mechanism are missing, not the whole criterion. |
| C3 | **2.7** cites *"The 3GPP review recommended GPS-time calibration."* No such review exists in the repo. | Remove the dangling reference; the underlying gap (no calibration protocol) is valid on its own. |

---

## Master findings table

### Workstream A — Contracts (root cause: schemas undefined) → dev-plan Phase 0

| ID | Finding | Pri | Δ | Resolution |
|---|---|---|---|---|
| 1.1 | Test plan / flow configuration format undefined | **P0** | — | Define `test_plan` + `flow` JSON Schema in `contracts/`; it is the payload core for `RUN_TEST` and the source for `header.json`. |
| 1.2 | `RUN_TEST` command payload undefined | **P0** | — | Define in `contracts/` as `{test_id, test_plan, flows[], per-flow server_host/port, duration, logging_config_override?}`. |
| 1.3 | Upload protocol underspecified (body, compression, sequence, resume, auth) | **P0** | — | Define upload protocol: summary `POST` first, then file upload; choose body format (recommend per-file `PUT` + manifest, zstd), define retry/resume semantics. |
| 1.4 | `supported_features` vs `capabilities` redundancy | P1 | — | Drop `supported_features`; `capabilities` is the single source. (Note: AGENTS.md **already** omits it — see M1.) |
| 2.8 | Port allocation not connected to flow config | P1 | — | Define allocation = **one port per flow**, carried per-flow in the `RUN_TEST` payload (folds into 1.2). |
| 3.2 | `header.json` contents vague | P2 | — | Provide concrete `header.json` schema (= test_plan + device metadata + version fields), generated from the same contract as 1.1. |
| 3.3 | No database schema | P2 | — | Define core tables (devices, sessions, tests, flows, upload_state) as sqlx migrations under `contracts/`/`server`. |
| 3.4 | Internal API (test-control ↔ data-generator/server-main) undefined | P2 | — | Define transport + message schemas (recommend internal gRPC or local HTTP); document `PREPARE_TEST`/`START_GENERATOR`/etc. payloads. |
| 3.6 | Device `status` payload underspecified | P2 | — | Define `POST /status` body: logger state, current test_id, `logging_config_version`, battery, free storage, radio snapshot. |
| 3.7 | Controller-facing REST API entirely missing | **P1** | ↑ from P2 | Add controller endpoints (list devices, create/start/stop test, query status, trigger upload). Blocks the Controller MVP outright. |
| M4b | Test-result reporting: `COMPLETED`/`FAILED`/`PARTIAL` states exist (§14.2) but no endpoint or rule to report them, and PARTIAL is undefined | P1 | NEW | Add result field to summary upload; define what makes a test PARTIAL (e.g., ran but lost ≥X% packets / lost coverage). |
| M3 | No versioning/compatibility policy for the 4 version fields | P1 | NEW | Document when each of `schema_version`/`protocol_version`/`app_version`/`logging_config_version` bumps and the compatibility window (see development_plan §4). |

### Workstream B — Measurement semantics (data-correctness spec edits) → dev-plan Phases 1–2, 4

| ID | Finding | Pri | Δ | Resolution |
|---|---|---|---|---|
| 2.2 | Bidirectional **Bulk TCP** on one connection biases throughput (ACK cross-traffic) | P1 | — | Specify Bidirectional TCP as **two separate connections** (one UL, one DL). Standard, removes the bias. |
| 2.3 | Bidirectional **Realtime UDP** asymmetric/ambiguous | P1 | — | Model as **two unidirectional flows** with distinct `flow_id`s, each with own rate/size (see C1). |
| 2.5 | `neighbor_cells` listed as *derived* but directly available via `getAllCellInfo()` | P1 | — | Move to **logged** primary fields in `radio.parquet`. (AGENTS.md already does this — see M1.) |
| 2.6 | SIGNAL_LOSS heuristic incomplete (`N`, mechanism) | P1 | — | Define: trigger when `ServiceState`=OUT_OF_SERVICE **or** `getAllCellInfo()` empty for `N` s; set default `N`. |
| 2.7 | Clock-calibration protocol absent (offset/uncertainty unexplained) | P1 | — | Add §11.4: GPS-time sampling + correlation, optional probe-burst calibration, uncertainty estimation method. |
| 2.1 | "Every file contains metadata" conflicts with the `header.json`-centric layout | P1 | — | Rule: metadata lives in `header.json`; Parquet rows carry only `device_id`/`session_id`/`test_id` as join columns. |
| 4.2 | `actual_interval_ns` measurement point ambiguous | P2 | ↑ from P3 | Define `actual_interval_ns = recv_mono_ns[i] − recv_mono_ns[i−1]` over **consecutive received** packets (gaps preserved). |
| 4.3 | `packets.parquet` has no send/receive direction | **P1** | ↑ from P3 | Add `direction` (`SENT`/`RECEIVED`). Without it, bidirectional data is uninterpretable — correctness issue. |
| 4.4 | `events.parquet` carries no per-event metadata | P2 | ↑ from P3 | Add `event_metadata` JSON column (HANDOVER old/new PCI, SIGNAL_LOSS duration, etc.). Derived events depend on it. |
| M4a | `ProbePacket.send_time_ns` doesn't say mono vs UTC | P2 | NEW | Specify **monotonic** (RTT is measured round-trip at the sender; mono is correct). |
| 4.5 | Possible missing `network.parquet` (NR↔LTE, IP/interface changes) | P3 | — | Optional: add a network-events file or fold into `events.parquet` via `event_metadata`. Defer unless needed. |

### Workstream C — Consistency & hygiene → cross-cutting, do alongside A/B

| ID | Finding | Pri | Δ | Resolution |
|---|---|---|---|---|
| M1 | **Cross-document drift** across spec / AGENTS.md / development_plan.md | P1 | NEW | Reconcile: spec is the outlier on `supported_features` (only spec has it) and `neighbor_cells` (spec=derived, AGENTS=logged). Fix spec, then re-sync all three per the "keep docs in sync" rule. |
| 2.4 | "traffic generator client" vs "traffic generator / receiver" naming unclear | P2 | — | Rename to explicit roles (flow coordinator / mobile generator / mobile receiver) or merge into one direction-parameterized component. |
| 3.1 | Flow-direction table is uninformative (all rows identical) | P2 | — | Replace with payload-origin/destination/initiator table (symmetry is intentional, but show what *differs*). |
| 3.5 | §16 MVP phrasing outdated ("generator for ToServer/Bidirectional" implies asymmetry) | P2 | — | Update §16 to match the symmetric §8.3 model. |
| 4.1 | "client" / "UE" / "mobile" used interchangeably | P3 | — | Standardize: **UE/Mobile** = device, **Logger** = the app, avoid bare "client." |

### Workstream D — Security → dev-plan Phase 5 (design hooks now)

| ID | Finding | Pri | Δ | Resolution |
|---|---|---|---|---|
| 3.8 | No authentication on register/command/upload | P2 | — | **Resolved (designed):** bearer-token-over-TLS model — operator JWT + per-device enrollment token, one middleware (`development_plan.md` §7, spec §5.3). Implementation pending. |
| M5 | `device_id` is **self-asserted** at registration (spoofing/trust) | P2 | NEW | **Resolved (designed):** device enrollment issues a server-bound `device_id`; identity derived from the token, not the client claim (`development_plan.md` §7, spec §5.3). Implementation pending. |

---

## Execution plan (sequenced)

The order minimizes rework: fix the cheap trust/consistency issues, then write the blocking contracts, then the correctness edits, then polish.

**Step 0 — Free wins (hours).** C1–C3 (fix the review's own errors) + M1 (reconcile the three docs; the spec is the outlier). Unblocks nothing technical but stops implementers chasing phantom text and divergent schemas.

**Step 1 — P0 contracts (the actual blockers).** 1.1 → 1.2 → 1.3, plus 2.8 and M4b/M3 which fold into them. These are Phase 0 of `development_plan.md`; nothing in `test-control`, the mobile test runner, or upload can start until they exist. Land them as files in `contracts/` with golden fixtures.

**Step 2 — P1 measurement semantics + remaining P1 contracts.** 2.1, 2.2, 2.3, 2.5, 2.6, 2.7, 4.3 (data correctness); 1.4, 3.7 (contracts). Do these before/with Phase 1–2 coding so the wire formats and log schemas are right the first time.

**Step 3 — P2.** 3.2, 3.3, 3.4, 3.6 (contracts); 4.2, 4.4, M4a (semantics); 2.4, 3.1, 3.5 (hygiene); 3.8, M5 (security design hooks). Mostly needed before the corresponding component is built, not before *any* code.

**Step 4 — P3 polish.** 4.1 terminology, 4.5 network.parquet (only if a need appears).

---

## Readiness re-rating

`design_review.md` rated the spec **70% ready**. Adjusted: **~55–60%.** The unblocked ~40% (control-plane skeleton + UDP probe echo) is real, but `test-control` — a core component — is fully blocked by the three P0 contracts (1.1/1.2/1.3). Readiness rises sharply once Step 1 lands.

---

## Status tracking

| Workstream | Items | Done |
|---|---|---|
| Corrections | C1, C2, C3 | ☐ |
| A — Contracts | 1.1, 1.2, 1.3, 1.4, 2.8, 3.2, 3.3, 3.4, 3.6, 3.7, M3, M4b | ☐ |
| B — Semantics | 2.1, 2.2, 2.3, 2.5, 2.6, 2.7, 4.2, 4.3, 4.4, 4.5, M4a | ☐ |
| C — Consistency | M1, 2.4, 3.1, 3.5, 4.1 | ☐ |
| D — Security | 3.8, M5 | ◑ designed (impl pending) |
