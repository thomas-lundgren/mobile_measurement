# Application workloads over shared traffic engines — assessment and plan

**Status:** accepted with adjustments, 2026-10-20; **A done** (2026-10-20:
`core/src/exchange.rs`); **A′ done** (2026-10-21: engine without names or
rows, `exchange::Pool`, `core/tests/layering.rs`); **B1 done** (2026-10-21: model
`StepMeta`, `scheduler` / `executor`, `app_events` log); **B2 done**
(2026-10-21: `app.tasks` / `app.intent`, Tasks and planned-vs-observed tables
on the findings page); **C1 done** (2026-10-21: `app_traffic/graph.rs`,
`executor::run_graph`, profile `websearch`); **C2 done** (2026-10-21: `loop`,
`task_interval_s`, `max_open_tasks`; `Agent::next_task`, `executor::run_timed`;
lateness and dropped tasks on the page); **D0 done** (2026-10-21: packet
layouts generated from `contracts/packets/layouts.yaml`); **D1 done** (2026-10-21:
`engines`, `Engine::{Exchange, Bulk}`, paced uploads); **D2 done** (2026-10-21:
`AppBurst*` packets generated from `layouts.yaml`, `core/src/burst.rs`,
`Engine::UdpBurst`, the flow's port over UDP); **D3 done** (2026-10-21: `dscp`
per activity, burst analysis and page table, video phases); **E1 done**
(2026-10-22: parameter registry `contracts/app_traffic/parameters.yaml` →
generated `params.rs`, version in header.json); **E2 done** (2026-10-22:
profiles chat, visual, image_gen, video_understanding). Next: E3 (server
packet capture). Supersedes the step order in
`ai_traffic_implementation_plan.md` (see its "Order" section).

## 1. The proposal (summary)

Separate *what a simulated application does* from *how bytes are moved*:

1. **Workload models** — web, mail, video, social, mixed, agent, script:
   operations with sizes, directions, timing, parallelism, dependencies,
   seeds. No sockets.
2. **Workload scheduler** — timing, concurrency, dependencies, cancellation,
   retries, engine selection; open-loop and closed-loop.
3. **Reusable traffic engines** — TCP transfer, UDP stream, request/response,
   echo/probe, DSCP marking; shared by app workloads and standalone tests.
4. **Measurement** — common identifiers and timestamps for intended
   operations, socket transfers and observed packets; diagnostic traffic kept
   distinct from application traffic.

Phases: consolidate (extract shared TCP from `app_traffic`), separate models
and scheduling, support composition, then richer AI models.

## 2. Assessment against the code

**Already true**

- Models are socket-free and unit-tested: `core/src/app_traffic/model.rs`
  emits `Step`s; its tests open no sockets.
- Composition at the flow level exists: a test plan holds several flows
  (head-of-line cycles run realtime UDP + bulk TCP + `app_traffic` together).
- Diagnostic traffic is separate: probe, realtime UDP and DSCP probe are their
  own flows and rows.
- Realtime UDP and bulk TCP loops are shared by both nodes
  (`core/src/endpoint.rs`).

**The real gap**

- The **request/response engine** is not shared. The UE side (connection pool,
  `AppExchange` request, upload, think time, paced download) lives in
  `app_traffic/runner.rs`; the server side in `data-generator/src/flow.rs`
  (`serve_app_connection`), which imports `read_exact_until` /
  `write_all_until` from `app_traffic` — the server depends on the app layer.
- The runner mixes **scheduling and execution**: `worker()` / `burst()`
  decide timing, parallelism and waiting *and* do the I/O.

**Adjustments to the proposal**

1. *Bulk TCP and request/response stay two engines.* Bulk is one continuous
   stream; an exchange is framed request/response. They share primitives
   (connect with timeout, read/write until a deadline, `bulk_pacing`), not a
   loop.
2. *Measurement instruments stay outside the scheduler.* Probe and realtime
   UDP keep their own high-priority threads and pacers. "UDP for an app
   operation" is a separate paced-burst engine, never the realtime measurement
   loop, so app scheduling cannot skew the instruments.
3. *Names.* "Session" is already the platform's `ses_…` and "flow" a test-plan
   flow. New concepts: **workload**, **agent** (app session), **task**,
   **operation**, **transfer**.
4. *"Priority" must be concrete.* Options: DSCP per operation (TOS on that
   connection; observed, not assumed to map to QoS), order inside the
   scheduler, or deadlines counted as missed. Each experiment names which.
5. *Per-operation UDP needs a server.* A UDP request/response or burst
   service does not exist yet; it belongs to the composition phase, not the
   first one.
6. *"Equivalent workloads" is checked precisely:* the seeded intended
   operation sequence before vs after is identical (models are
   deterministic); the data-generator `app_traffic` integration test passes
   unchanged; one live phase compares per-activity bytes and timing.

## 3. Plan

| | Work | Replaces (AI plan) | Effort |
| --- | --- | --- | --- |
| **A** | Extract the request/response engine (done) | new | ~1 day |
| **A′** | Finish the separation: activity table to the codec, rows out of the engine, `exchange::Pool`, layering gate (done) | new | ~½ day |
| **B** | Scheduler apart from execution; operation record with IDs; intended-event log | step 1 | 1–1.5 days |
| **C** | Open loop and dependency graphs; `agentic_websearch` | step 3 | ~1.5 days |
| **D** | Engine per operation: exchange, bulk segment, paced UDP burst (+ server side); DSCP per operation | new | 1.5–2 days |
| **E** | Parameter registry; F2–F4 models; server packet capture | steps 2, 4, 5 | as planned |

### A — Extract the request/response engine (first)

Pure consolidation; no behaviour or wire change.

- New `core/src/exchange.rs` beside `endpoint.rs`:
  - client: `connect(server)` (timeout, socket timeouts, `TCP_NODELAY`),
    `Pool` (per-agent idle connections, `IDLE_CLOSE`), `transfer(conn,
    AppExchange, observer)` — request, upload chunks, wait, read the response
    or ack — reporting each read/write to an observer (the runner turns that
    into packet rows; B adds event timestamps there);
  - server: `serve(conn, flow_id, sink, counters, stop)` — today's
    `serve_app_connection`, v1 and v2 requests, think time, paced response;
  - shared: `read_exact_until`, `write_all_until`, `give_up`.
- `app_traffic/runner.rs` keeps agents, bursts and rows, calls `exchange`.
- `data-generator/src/flow.rs` `spawn_app_server` calls `exchange::serve`;
  no more `use fieldtest_core::app_traffic` in the server.
- Checks: the seeded operation sequence is unchanged (model untouched);
  `server/data-generator/tests/app_traffic.rs` passes unchanged; a new unit
  test drives `exchange::transfer` against `exchange::serve` on localhost
  (download, upload with ack, paced stream timing); `ci/check.sh` green; one
  live app phase on cellular, per-activity bytes/timing compared with an
  earlier run.

### A′ — Finish the separation (2026-10-21 review of A)

A moved the exchange into one engine, but the engine still carried app
vocabulary and the runner still held the pool:

- `exchange.rs` owned the activity name table (`ACTIVITIES`, "llm_call", …)
  and built app-tagged log rows itself (`exchange::row`, pushed by `serve`).
- The connection pool (open, reuse, close after `IDLE_CLOSE`) lived in
  `app_traffic/runner.rs` as a bare `Vec<TcpStream>`.
- Nothing enforced the direction: the data-generator dropped its
  `app_traffic` import in A, but no test kept it that way.

Work:

- The activity table moves to `packet.rs` beside the `AppExchange` codec (it
  is a wire field: code = index of `enums.yaml` `app_activities`); its
  contract test moves with it.
- `exchange` reports I/O to an observer on **both** sides (`transfer` already
  did; `serve` now too) and neither builds rows nor touches the log sink. The
  exchange → packet row mapping is `PacketRow::exchange` in `log_writer` (the
  measurement layer), used by the UE runner and the data-generator.
- `exchange::Pool`: per-agent idle connections, opened on demand, closed after
  an idle time the workload chooses.
- `core/tests/layering.rs`: the engines (`endpoint`, `exchange`, `packet`,
  `udp_timing`, `log_writer`) and the data-generator / test-control sources
  must not name `app_traffic`. Tests may (they drive a UE).

**Layers and the rule they keep.** Engines move bytes and report what moved;
workloads (`app_traffic`) decide what to move and when; orchestration (`ue.rs`
on the phone, `flow.rs` in the data-generator) wires flows to engines. The
server side has no workload knowledge: everything it does for an exchange is
dictated by the request on the wire (bytes up, think time, bytes down,
pacing). The planned `udp_burst` server (D) follows the same rule.

### B — Scheduler apart from execution

Split in two. **B1 (done):** the models tag every burst with `StepMeta`
(task, step, kind; no random draws, so the seeded transfers are unchanged —
checked by hashing every fetch before and after); `app_traffic/scheduler.rs`
turns bursts into `Operation`s and writes `mobile/app_events_<flow>.parquet`;
`app_traffic/executor.rs` runs a burst through `exchange` and reports a
`Done` (start, first byte, end, bytes, outcome, new connection) per
operation. Contract: `parquet/app_events.parquet.yaml`, `enums.yaml`
`app_step_kinds` / `app_outcomes`, the upload path. **B2 (done):**
`analysis` `app.tasks` (per kind of task: started, finished, task time,
steps, objects not ok) and `app.intent` (per activity: wait for a connection,
planned think against observed first byte, planned against observed stream
length, outcomes, new connections); both on the findings page's App traffic
tab.

- `Operation { id, agent, task, step, kind, activity, engine, fetch,
  after: Vec<id>, parallel_group }` emitted by the models (today's `Step` +
  `StepMeta` from the AI plan's step 1).
- Scheduler (closed loop, as today): runs an operation when its `after` set is
  done, up to the parallel limit, through the chosen engine; cancellation on
  stop; timeouts; `skipped` when no connection.
- The intended-event log `mobile/app_events_f<flow>.parquet` (columns as in
  `ai_traffic_implementation_plan.md` §1.3) is written by the scheduler.
- Analysis and page: Tasks table, planned vs observed per activity (AI plan
  §1.5).

### C — Task graphs and open loop

As AI plan step 3, in two parts. Built for the agent workloads now, but kept
free of app types (`app_traffic/graph.rs`: nodes, dependencies, the ready
set) so it can become the generic scenario layer for every test later
(timed load segments, ramps now driven by shell scripts — evaluated
separately).

**C1 — graphs, closed loop.**
- Models may emit a task as a graph: nodes (a fetch, its step meta) with
  dependencies. Existing profiles keep emitting bursts; a burst is a graph
  without dependencies, so there is one execution path.
- The executor runs a graph: at most `parallel` in flight, a node starts when
  all its dependencies ended `ok`; a node whose dependency failed is logged
  `dep_failed` and never runs. Connections are taken from the pool per
  worker as work arrives (not all up front).
- Profile `websearch` (`agentic_websearch`): an LLM plan call → a search API
  call → 2–6 page fetches in parallel → an LLM synthesis call that waits for
  all fetches (their text goes up as context); 30 % a second search round;
  5–60 s between tasks. Cloud placement: the search and fetches run at the
  provider (a `cloud_tool` pause and `llm_resume`); hybrid: search at the
  provider, fetches on the phone. Sizes and times are estimates marked
  unverified until the registry (E).
- Contract: `app_activities` + `web_search` (appended, wire code 14);
  `app_step_kinds` + `search`, `fetch`; `app_outcomes` + `dep_failed`;
  `app_events.after_objects` (list of the object ids a node waited for);
  profile `websearch`.
- Checks: graph shape and determinism (unit); existing profiles' fetch hash
  unchanged; executor dependency order and `dep_failed` (unit, local
  server); data-generator end-to-end with `websearch`; live phase
  `app-websearch`.

**C2 — open loop.**
- Flow field `loop: closed | open` (default closed). Open: tasks arrive at
  random with a mean interval (`task_interval_s`), whatever is still
  running; inside a task each node is due at the task start plus its planned
  offset (think time + transfer at a nominal rate) and starts then, late
  dependencies or not. A full backlog (unfinished tasks per agent) drops the
  arriving task as `skipped`, so offered load stays visible.
- `app_events.due_mono_ns` (open loop): lateness = start − due.
- Analysis: lateness, offered vs carried load, tasks finished in the fixed
  duration; closed vs open on the same seed side by side. Phase
  `app-websearch-open`.

### D — Engine per operation

Each app operation picks how its bytes move; the models stay transport-free
(the test plan maps activity → engine). Each engine is also a building block
for the generic scenario layer (a bounded bulk segment is a timed load step).

**D0 — packet layouts generated from one source (first).** Today the
layouts are hand-written Markdown, the Rust codec hand-written to match,
`validate_contracts.py` packs vectors with its own struct formats, and the
README's type registry had drifted (0x03, 0x04 missing). Instead:
`contracts/packets/layouts.yaml` (the type registry and every layout:
`pkt_t`, `ver`, ordered fields with type and doc); `contracts/codegen/packet_codec.py`
generates the Rust codec (structs, encode/decode, sizes, type constants) and
the Markdown layout tables, with `--check` in CI; the CI vector check builds
its struct formats from the YAML. Golden vectors stay hand-authored, so they
remain an independent oracle. The existing fixed-layout packets (probe,
realtime, dscp_probe, app_request v1, app_exchange v2) move first — their
vectors prove the generated codec equals the hand-written one. Reach and
time sync (text line, unframed reply) keep their own vectors.

**D1 — engine per operation, bulk segment (no wire change).** Flow field
`engines` (activity → `exchange` | `bulk` | `udp_burst`, default
`exchange`); `app_events.engine`; `enums.yaml` `app_engines`. `bulk`: one
bounded transfer on its own connection (not the pool), uplink paced on the
phone; same wire and server handler as `exchange`.

**D2 — paced UDP burst.** Packets (YAML → generated codec, vectors): burst
request (object id, direction, count, size, rate, DSCP), data (object id,
index, send mono/utc), end, ack (count received). Engine `core/src/burst.rs`
(engine layer, layering gate): downlink — the phone's request (retried),
the server sends `count` paced datagrams to the phone's address; uplink —
paced datagrams, an end marker, the server's ack. The app flow's port also
opens for UDP; the server does what each request says. Both nodes log every
datagram (kernel receive time, `packets.app_packet_index`); a burst that
ended is `ok`, loss shows as `app_events.packets` against `plan_packets`.

**D3 — DSCP per operation, analysis, experiments.** Flow field `dscp`
(activity → 0–63): UDP bursts marked both ways and the received marking
logged on both nodes; TCP marked on the phone's uplink only (downlink marking
and TCP observation need a wire change and capture: E). Analysis and page:
per engine completion; bursts' loss, inter-arrival jitter against the
planned spacing, DSCP sent vs received; same-seed comparison. Phases
`app-video-tcp`, `app-video-udp`, `app-video-udp-ef`.

### E — As planned

Parameter registry (AI plan step 2), F2–F4 (step 4), server packet capture
(step 5).

Also open from `mobile_ai_traffic_generator_development_plan.md` §11:
`agentic_computer_control` (small frequent commands, screenshot uploads);
traffic-shape metrics (UL:DL per family, burst peak-to-mean, CV — define
them first); distributions beyond lognormal; per-run provenance (parameter
set + seed); AI agents writing `script` timelines.

## 4. Principles kept

One implementation per transport behaviour; one measurement framework;
models independent of transport; closed-loop effects visible; existing seven
profiles remain the regression baseline; synthetic traffic is never presented
as the real protocol; seed, intended operations, transfers and observations
are traceable.
