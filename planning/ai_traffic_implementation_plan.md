# AI traffic — implementation plan

**Status:** planned 2026-10-20; step 1 is ready to implement.
**Context:** `mobile_ai_traffic_generator_development_plan.md` §11 lists what
the platform's `app_traffic` agent already covers and what is missing. This
plan turns its suggested order into implementation steps inside this
repository (shared Rust core, Logger, data-generator, analysis, findings page).

Principles for every step:

- **No wire change unless needed.** The server and the UE already log every
  object under the same `object_id`; anything only the UE knows (task, step,
  intent) goes into a UE-side log and is joined on `object_id`.
- **Nothing extra on the hot path.** New logs are low-rate (one row per
  object, not per packet) and are written when the test stops, not during it,
  like `gps.parquet` (`RustCore` session buffers).
- **Additive contracts.** New Parquet table and columns, no `schema_version`
  bump (invariant 4); a new profile or flow field is optional.
- **Intent and observation stay separate** (plan invariant 3): packet rows
  remain what the sockets saw; the new table holds what the model meant to
  do and how each exchange ended.

---

## Step 1 — Task/step IDs and the intended-event log

**Goal.** Every app exchange can be attributed to *agent → task → step →
object*, and analysis can compare what was intended (bytes, think time,
pacing, dependency) with what happened (start, first byte, completion,
outcome). Unlocks per-task metrics (task time, steps per task, tasks done
in a fixed time — the plan's closed-loop comparison).

### 1.1 Identifiers

| ID | Meaning | Source |
| --- | --- | --- |
| `test_id` | the run | existing |
| `agent` | simulated user (worker index) | existing (`object_id` bits 48–63) |
| `task` | one unit of user intent, per agent, from 1: an agent task (LLM loop until the answer), a page view, a mail sync, a video session, a feed scroll | new, from the model |
| `step` | the burst's position within its task, from 1 (agent: LLM call 1, tools of call 1, LLM call 2, …) | new, from the model |
| `episode` | the burst, per agent (already the middle bits of `object_id`) | existing |
| `object_id` | one exchange (request → response/ack) | existing, the join key with `packets` rows on both nodes |
| `after_episode` | the burst this one waited for (closed loop: the previous burst of the same agent; none for the first) | new |

`object_id` keeps its layout (`agent << 48 | episode << 20 | index`), so the
server, the wire format (`AppExchange` v2) and old analysis stay unchanged.

### 1.2 Model changes (`core/src/app_traffic/model.rs`)

- `Step::Burst { fetches, parallel }` gains `meta: StepMeta { task: u32,
  step: u16, kind: &'static str }`. `kind` names the step for the log
  (`llm_turn`, `tools`, `cloud_tools`, `page_html`, `page_objects`,
  `mail_sync`, `video_start`, `video_segment`, `feed`, `script_action`).
- `Agent` keeps `task: u32` and `step: u16`: a new task starts where the
  model already starts one (agent: `turns_left == 0` branch; web: `web_page`;
  mail: `mail`; video: a new viewing session; social: `feed`; script: each
  action is its own task). Bursts queued in `self.queue` carry the meta of the
  task that queued them.
- Deterministic: same seed → same (task, step) sequence. Unit test.

### 1.3 Runner changes (`core/src/app_traffic/runner.rs`)

Per object, an `AppEventRow` (in memory, `Mutex<Vec<_>>` shared by the
agents of the flow):

| Column | Type | Meaning |
| --- | --- | --- |
| `flow_id`, `object_id` | u32, u64 | join keys |
| `agent`, `task`, `step`, `episode`, `after_episode` | u32/u32/u16/u64/u64? | §1.1 |
| `step_kind`, `activity` | str | §1.2; activity name as in packets |
| `parallel` | u8 | objects in flight allowed in this burst |
| `plan_up_bytes`, `plan_down_bytes`, `plan_delay_ms`, `plan_rate_bps`, `plan_chunk_bytes` | u64/u64/u32/u32/u32 | the `Fetch` as the model asked for it (×scale) |
| `planned_mono_ns` / `planned_utc_ns` | u64 | when the model emitted the step |
| `start_mono_ns` / `start_utc_ns` | u64 | request write began (after waiting for a pooled connection) |
| `first_byte_mono_ns` / `first_byte_utc_ns` | u64? | first response byte (or the ack) |
| `done_mono_ns` / `done_utc_ns` | u64? | last byte / ack |
| `bytes_up`, `bytes_down` | u64 | what actually moved |
| `outcome` | str | `ok`, `timeout` (OBJECT_TIMEOUT), `stopped` (test end), `error` (connection lost), `skipped` (no connection: burst dropped) |
| `new_connection` | bool | this object opened its TCP connection (handshake + slow start cost) |

Timestamps are dual-clock (invariant 2). Rows are pushed when the object
ends (any outcome), plus `skipped` rows for a burst that could not get a
connection — today that burst silently disappears.

Writing: `app_traffic::run` gets the flow's `log_dir`; after its agents join
it writes `mobile/app_events_f<flow>.parquet` (one file per app flow) via a
new `aux_log::write_app_events`. Same pattern and failure handling as
`write_gps` (logging-first: failure loses only this table).

### 1.4 Plumbing

- `core/src/ue.rs`: pass `cfg.log_dir` into `AppTraffic` (new field).
- `core/src/lib.rs` `sealed_files`: include `app_events*.parquet`, so the
  Logger uploads it (no Java change: the sealed list drives the upload).
- `contracts/parquet/app_events.parquet.yaml`: the table above; README.
- New test: the Rust writer's schema equals the YAML (column names, types,
  nullability). There is no such test for the other tables yet; start the
  pattern here and add `packets` to it.
- Spec: App Traffic section (identifiers, the table); `contracts/packets/app_request.md`:
  note that IDs live in the log, not on the wire.

### 1.5 Analysis (`analysis/fieldtest_analysis`)

- `schema.APP_EVENTS`; `loader` reads `mobile/app_events*.parquet` into
  `TestData.app_events` (empty frame for older tests).
- `app.tasks(td)`: per task — agent, kind, steps, objects, start → end,
  MB up/down, outcome mix. Per test: tasks completed, task time p50/p90,
  steps per task, share of objects not `ok`.
- `app.intent_check(td)`: per activity, planned vs observed — down bytes
  (should match), server think (planned delay vs observed first-byte minus
  last-byte-up), stream duration (planned bytes/rate vs observed), and how
  long bursts waited for a connection (`start - planned`).
- `series` → day files: `app.tasks` summary per test (not per task).
- Findings page, App traffic tab: a "Tasks" table (tasks done, task time
  p50/p90, steps/task, objects not ok) and a planned-vs-observed line per
  activity. Older tests show "—".
- Bump `SERIES_VERSION`.

### 1.6 Tests and acceptance

- Unit (model): same seed → same `(task, step, kind)` sequence; agent task
  boundaries where `turns_left` resets; placement cloud keeps the step order.
- Integration (`server/data-generator/tests/app_traffic.rs`): a scripted
  user writes `app_events_f<flow>.parquet`; one row per object with
  `outcome = ok`, byte counts equal to the plan, first byte ≥ planned delay;
  stopping mid-object gives `stopped`.
- Analysis: synthetic `app_events` → `tasks` / `intent_check` numbers.
- Live: one `app-agent` phase on cellular (app flows need more than the
  one port that works from home Wi-Fi). Check the page's Tasks table.
- `ci/check.sh` green.

**Effort:** about one working day. **Risk:** low — no wire or packets-schema
change, nothing on the per-packet path.

---

## Step 2 — Evidence registry for profile parameters

**Goal.** Every number the traffic models use has a recorded source and
status, as plan §A / invariant 4 require; today they are engineering
estimates in code.

- `contracts/app_traffic/parameters.yaml`: one entry per parameter —
  `id` (e.g. `agent.ttft_s`), value / distribution (lognormal median, sigma,
  clamp), unit, `profile`, `source` (`estimate` or a TDoc id + table/figure),
  `status` (`unverified`, `verified`, `project_choice`), measurement boundary,
  note. Start with all current values marked `estimate` / `unverified`; add
  the plan's v0.1 anchors (≈310 B text events, 54 B ACKs, 3.9 s visual comb,
  F1 UL:DL 15–30) as `reported, unverified`.
- `core/src/app_traffic/params.rs`: the constants, each referencing its id;
  a parity test (like `contracts-rs/tests/parity.rs`) fails when YAML and code
  disagree.
- `header.json`: the profile parameter-set version (a hash of the YAML) per
  app flow, so a result names the parameter set it ran with. Ties into the
  OTA plan's versioning.
- Findings page: an "unverified parameters" note on app results.

**Effort:** half a day for the mechanism; sourcing the numbers is research
(the plan's Phase 0), done alongside.

---

## Step 3 — `agentic_websearch` as a task graph; open-loop mode

**Goal.** The plan's minimal acceptance demonstration (§9): request →
response → dependent parallel web/tool transfers → follow-up context →
final answer, closed loop and open loop.

- **Task graph** (`model.rs`): `TaskGraph { nodes: Vec<Node> }`, `Node {
  id, kind, fetch: Fetch, deps: Vec<id> }`. Built by a generator per profile
  (`websearch`: LLM plan → search API call → 2–6 parallel page fetches →
  LLM synthesis → optional second round → answer) — sizes from step 2's
  registry.
- **Executor** (`runner.rs`): closed loop — a node starts when all deps
  completed (`ok`), up to `MAX_PARALLEL`; a failed dep fails its dependants
  (`outcome = dep_failed`). Open loop — every node starts at its scheduled
  offset (from the graph's planned durations), whatever happened before;
  late completions are logged, not waited for.
- Contract: `app_traffic.loop: "closed" | "open"` (default closed), profile
  `websearch`. `app_events` rows gain `node_id` and `deps` (additive).
- Analysis: per task, completed vs planned nodes in a fixed duration;
  closed vs open comparison on the same seed — the plan's key comparison.
- Scripts: `app_test.sh` phases `app-websearch`, `app-websearch-open`.

**Effort:** 1.5–2 days.

---

## Step 4 — F2 chat, F3 visual assistant, F4 content generation

Each a model in `model.rs` with its parameters in the registry, its own
activity names, a phase in `app_test.sh`, and a unit test of its shape.

- **F2 `chat`:** prompt up (0.3–4 kB), time to first token, streamed reply
  (event size from the registry: 140 B today vs the reported ≈310 B — make it
  a parameter), reading/typing pause; conversation context grows like the
  agent's but without tools.
- **F3 `visual`:** a periodic comb — an image/frame upload every period
  (reported 3.9 s, parameter), 50–500 kB, followed by a short streamed
  analysis reply; sessions of N periods. Needs a scheduled (open-loop-like)
  start per period, so it builds on step 3's executor.
- **F4 `image_gen`:** request (≈1 kB), long server processing (5–30 s),
  progressive previews then a final image (0.5–3 MB). **`video_understanding`:**
  large upload (5–50 MB, chunked) then a streamed answer.
- Server: no change (the `AppExchange` server already handles up / think /
  paced down).

**Effort:** 1–1.5 days for the four models; calibration follows the registry.

---

## Step 5 — Server-side packet capture

**Goal.** See what TCP actually puts on the wire next to the app log: ACK
sizes and rate, retransmissions, segment sizes, coalescing — the plan's
"actual transport packets" view (invariant 3) and the reported 54 B ACKs.

- test-control starts a capture per test on the server for the test's app
  and bulk ports: `dumpcap`/`tcpdump` with a header-only snap length
  (≈96 B), BPF filter on the allocated ports, into
  `server/capture_f<flow>.pcap`; stopped with the generators. Needs
  `CAP_NET_RAW` on the capture binary (`setcap`), no root server process.
- Size cap per test and a config switch (default on for app/bulk flows).
- Analysis: `capture.py` reads the pcap (dpkt or scapy), per flow and
  direction — packet sizes, pure-ACK count and size, retransmissions, RTT
  samples from TCP timestamps if present; joins to app objects by time.
- Phone side stays without capture (VPN capture would perturb traffic, plan
  §F); mp-probe's qdisc counters remain the phone's view.

**Effort:** about one day. **Risk:** capture CPU on the server during 1 ms
streams — header-only, BPF-filtered capture is cheap, but check pacing
accuracy with and without it once.

---

## Order and checkpoints

| Step | Depends on | Checkpoint |
| --- | --- | --- |
| 1 IDs + intended-event log | — | Tasks table on the page for a live `app-agent` run |
| 2 Evidence registry | — (parallel to 1) | parity test; header names the parameter set |
| 3 websearch graph + open loop | 1 | same-seed closed vs open run, task completion compared |
| 4 F2–F4 | 2, 3 (F3 uses the scheduled executor) | one live phase per family |
| 5 Server capture | — | ACK/retransmission view for one app phase |

Step 1 first: it is the foundation for every per-task and intent-vs-observed
result that the later steps report, and it changes nothing on the wire or
on the per-packet path.
