# Mobile AI / Agentic Traffic Generator — Development Plan v0.2

**Status:** Proposed implementation roadmap  
**Baseline:** User-provided *SPEC: Mobile-AI / Agentic Traffic Generator (v0.1)* (2026-10-20)  
**Target:** Android UE and Linux network-side server over a live 5G network.

## 1. Mission and scope

Build a configurable, reproducible traffic generator that reproduces application-level traffic characteristics attributed to 3GPP RAN2 Mobile AI studies, while allowing live transport and radio behavior to shape actual packets. The generator does not emulate an LLM, the radio stack, or the core network. It generates realistic **traffic causes and dependencies**: prompts, streamed responses, web/tool interactions, multimodal uploads, content transfers, idle periods, and agentic progression. Only exchanges actually traversing the UE connection are counted as UE traffic.

The four baseline service families are **F1 agentic AI**, **F2 conversational LLM**, **F3 multimodal visual assistant**, and **F4 content generation/analysis**. Within F1, include general agent, shopping/web-search, and computer-control subprofiles; tool traffic is part of the initial proposal, not a newly invented traffic family.

### Research questions

- What packet/byte, burst, and transaction patterns do these workloads produce through a real UE connection?
- How does radio impairment affect observed delay, burst dispersion, application progress, queueing and completion time?
- What changes when tool traffic runs on-device, in-cloud, or in a hybrid arrangement?
- How do independently scheduled UL/DL events differ from causally dependent agentic workflows?
- What can be measured regarding RRC activity and power, without claiming unavailable baseband observability?

## 2. Architectural decomposition

```text
                      Scenario + evidence catalog
                                |
                  Orchestrator / Run Controller
                   /                       \
           Android UE runtime          Linux server runtime
                    \                     /
                 Shared scenario/event engine
                  | session / task FSM
                  | burst scheduler / statistical sampler
                  | dependency scheduler / tool events
                  | flow manager / classification / QoS
                  | transport adapters: TCP, QUIC, paced UDP
                  | application events + capture hooks
                                |
                  Trace merger and validator
                                |
                      Per-run research report
```

### A. Reference and profile registry

Maintain source-backed profile definitions separate from scenario choices. Each numerical anchor has source TDoc, meeting, contributor, observation/table/figure, original-document-verification status, metric definition, measurement boundary, method (capture/simulation/proposal/conclusion), applicability, and uncertainty. **Never label a contributor observation as a binding 3GPP requirement.** The uploaded v0.1 source index provides candidate references, but full original TDocs and chair conclusions are not all independently verified.

### B. Scenario schema and compiler

Define a versioned JSON or YAML scenario with: seed, profile IDs and overrides; UE/server role placement; execution location (UE/cloud/hybrid); sessions and step graphs; independent flows; direction; burst shape, sizes and gaps; processing delays; response dependencies; transport/ports; QoS tags; capture settings; run bounds; safety resource limits. Validate strictly, resolve defaults and distributions, then compile into an immutable *intended-event schedule* or a seeded *causal event graph*. Record resolved parameters in run metadata.

### C. Shared session/task engine

The same core runs on Android and Linux. A session owns a task graph with step types such as LLM exchange, web search, HTTP download, tool-call control, tool-data transfer, multimodal upload, report delivery, sync, wait, and context compaction. Each event records where execution occurs and whether it actually crosses the UE link. Support repeated, parallel, conditional and retryable actions. The network-facing byte payload is synthetic, while timing, direction and dependencies reflect the chosen workload.

### D. Burst engine

Emit application-level bursts carrying `session_id`, `step_id`, `transaction_id`, `burst_id`, scheduled time, size and direction. Support seeded fixed, lognormal and Weibull distributions (other families only when source calibration warrants them), multiple burst shapes including streamed small writes, two-stage transfers and periodic comb patterns. Separate *source offered bursts* from *packets actually observed*.

### E. Transport adapters

1. **TCP (first):** long-lived connections, real OS TCP congestion control, backpressure, sequence framing for synthetic messages.
2. **QUIC (later):** comparable flow modes and configurable streams, using real QUIC stacks and honest treatment of internal ACK visibility.
3. **Paced UDP (later):** deterministic packet transmission; synthetic ACK-like messages, if enabled, must never be presented as real TCP ACKs.

Define *application frame/message IDs* in a common payload header. Packet captures may see partial, coalesced, encrypted, retransmitted or resegmented data: do not presume a one-to-one correspondence between application messages and IP packets. Maintain separate app-event, socket-write and packet-capture views.

### F. Platform-specific runtimes

**Android:** Kotlin UI/service for start/stop, permissions, mobile network binding where supported, device/connectivity metadata, scenario selection, export; native or interoperable portable engine chosen after a small build spike. Test foreground service behavior, Doze/thermal influences, Android packet-capture viability (VPN capture can perturb traffic), and clean shutdown. Do not require root for the first milestone.

**Linux server:** CLI service exposing experiment control, scenario manifest, UDP/TCP listeners, packet captures and resource accounting. Support simultaneous UE sessions eventually, but first prove one client/one server session.

### G. Timing and observability

Log monotonic timestamps at both ends. Record time sync quality and uncertainty. Use round-trip/echo measurements as an in-band fallback; do **not** claim precise one-way latency when offsets are insufficiently constrained. Primary counters: intended burst rate/volume; transport socket byte delivery; PCAP observed packet counts, sizes and directions; application transactions; time to first response, completion and stalled time; correlations by step. Capture actual TCP ACKs via packet trace. Distinguish application data bytes, transport bytes and on-wire IP bytes.

### H. Analyzer and reports

Offline merge application events, server/UE logs and PCAPs. Compute UL/DL traffic volumes and ratios by boundary and by family; burst-size and gap distributions; P/M and CV **only after defining their exact formulas, sampling window and burst segmentation**; packet inter-arrival and size distributions; retransmissions where visible; transaction latency; task completion and per-tool contribution; reproducibility. Provide comparisons against source-derived references with explicit confidence and missing-reference flags.

## 3. Development phases and acceptance gates

### Phase 0 — Establish evidence and technical contracts

**Tasks:** Retrieve original RAN2 contributions and meeting conclusions; audit cited source anchors and reported traffic methodology; decide burst and ratio metric definitions; publish schema v1 and wire/event logging contracts; prototype Android/Linux common-core packaging, clock sync and capture method; define network test constraints.

**Exit gate:** Reference catalog identifies every anchor as independently verified or pending; unit conventions and flow boundaries are unambiguous; Android ↔ Linux feasibility spikes pass. Do not block foundational coding on a missing TDoc, but do not call unverified profiles 3GPP-conformant.

### Phase 1 — Minimal end-to-end system

**Tasks:** Linux server and Android client; transport TCP; framed synthetic byte stream; connection lifecycle; direction-specific transfers; scenario/run/flow identifiers; local JSONL or equivalent structured event log; packet captures; manual one-task execution; collect outputs.

**Exit gate:** Repeatable UL request→DL response over live mobile data; bytes verified end-to-end; both endpoint logs exported; no silent packet/app-frame conflation; stop/restart works safely.

### Phase 2 — Scenario runner and burst generation

**Tasks:** Versioned scenario format, validation, seeding, fixed/stochastic sizes and gaps, timer scheduling, burst shapes, multiple flows, scenario manifest; open-loop schedule playback; tests for statistical generation, event ordering and resource limits.

**Exit gate:** Same seed/scenario generates identical intended event schedule; paced execution accuracy and jitter reported; independent offline comparison shows targeted app-level size/gap distributions within documented tolerances.

### Phase 3 — Agentic workflow and tool traffic

**Tasks:** F1 graph execution with prompt/response, tool-call control and tool data, web-search/browser-like transfer templates, parallel calls, context growth/compaction, sync and idle gaps. Implement open-loop and **closed-loop** progression. Support execution-location topology so cloud-only tool transfers are excluded from UE-link traffic. Include per-activity volume/time attribution.

**Exit gate:** Example task demonstrates dependency gating, retries, parallel tool requests, compaction and correct accounting for traffic that actually traverses the UE. In closed-loop mode, delayed responses delay dependent steps.

### Phase 4 — Reference traffic-family profiles

**Tasks:** Implement F2 streaming chat, F3 periodic multimodal upload, F4 image generation/video understanding/reasoning and complete F1 general/web-search/computer-control configurations. Calibrate **at the correct measurement boundary**; allow within-session direction changes and source-reported alternative measurement sets. Avoid hardcoding averages as universal truth.

**Exit gate:** Each profile has reference IDs, calibrated input statistics, validation report, and declared uncertainty. Report *unverified evidence* prominently; do not claim packet patterns are matched simply because transport mode is real.

### Phase 5 — QUIC, paced UDP, QoS and concurrency

**Tasks:** QUIC transport, deterministic paced UDP, multiple transport flows, configurable DSCP tagging and observed DSCP, optional multi-session load, server concurrency. Document that DSCP does not imply a particular 5QI or distinct DRB on a commercial network.

**Exit gate:** Same high-level session can run using TCP/QUIC/UDP with differences explicitly measured; real packet captures and run manifest show transport selection; QoS/DSCP treatment is described as observed rather than assumed.

### Phase 6 — Field campaigns and research reporting

**Tasks:** Run tests across good/poor radio conditions, varying traffic mixes and UL/DL skews, open vs closed-loop control, with/without parallel tool transfers. Capture radio information available on the Android test device separately from app and packet measurements; include device modem logging only if independently supported by hardware/access. Analyze delay, stalls, burst deformation, throughput, optional RRC and power correlations, and task completion.

**Exit gate:** Reproducible experimental report containing input profiles, device/network circumstances, traces, clock-quality limitations, distribution fit, and causal interpretation of results. Negative results and measurement limits are retained.

## 4. Concrete initial profiles

| Profile | Key workload elements | Original v0.1 candidate evidence |
| --- | --- | --- |
| `agentic_general` | Multi-step UL/DL transactions, context growth, compaction, tool transfers | R2-2603786; R2-2607568; R2-2603760 |
| `agentic_websearch` | Search request, result, multiple page/data fetches, follow-up inference | R2-2603786; R2-2603760 |
| `agentic_computer_control` | Small frequent commands and responses, optional screenshots | R2-2603786 |
| `chat_llm` | Prompt followed by micro-streamed DL response | R2-2602611; SA4 source pending |
| `multimodal_visual` | Periodic sizeable UL uploads followed by DL analysis | R2-2602611; R2-2605020 |
| `image_gen` | Request, processing gap, multi-stage DL output | R2-2602611; R2-2602613 |
| `video_understanding` | Upload-intensive content plus analysis response | R2-2602613; R2-2603786 |

Numbers from v0.1 (e.g. F1 UL:DL 15–30, text streaming approximately 310-byte DL and 54-byte TCP ACKs, 3.9s visual-assistant comb period, F1 P/M=2 and CV=1.41) remain **provisional reported values**, conditional on verifying the original measurement methods and sources. `compaction.every_n_steps=10`, `ul_drop_fraction=0.8`, tolerance ±20/25%, and DSCP 46 in the JSON example are *scenario/project choices*, not sourced universal defaults.

## 5. Suggested codebase structure

```text
mobile-ai-traffic/
  docs/
    spec/  references/  protocol/  experiments/
  profiles/
    agentic/  llm/  multimodal/  content/
  schemas/
  core/
    scenario/ session/ graph/ burst/ flows/ telemetry/
  transport/
    tcp/ quic/ udp/
  platforms/
    android/ linux/
  control/
  analysis/
    ingest/ merge/ calibration/ reports/
  tests/
    unit/ integration/ replay/ field/
  experiments/
    manifests/ results/
```

This is a **logical** decomposition, not a binding choice of implementation language or repository structure. Choose shared-core technology after validating Android builds, networking and timing behavior.

## 6. Workstreams for parallel agentic development

- **Research/evidence agent:** Validate TDocs and claims, maintain the evidence catalog and source-context warnings; owns profile provenance, not software assumptions.
- **Architecture/contracts agent:** Own event IDs, scenario schema, binary/message framing, timing contracts and module boundaries.
- **Core engine agent:** Own seeded scheduling, FSM/task graph, correlated bursts and calibration unit tests.
- **Android agent:** Own client runtime, connection management, Android lifecycle and export, with no-root-first tests.
- **Linux/transport agent:** Own server, TCP/QUIC/UDP adapters, run control and end-to-end integrity.
- **Measurement agent:** Own event logging, PCAP interfaces, clock calibration, trace merge, validators and reports.
- **Integration/QA lead:** Own vertical slices, baseline regressions, field-test manifests and release gates. Resolve cross-team ambiguities using architecture contracts before merge.

Start with contracts and the first TCP vertical slice; do not simultaneously implement advanced profiles on top of changing wire and event schemas. A research manager should maintain a **decision log** of what was measured, what was assumed, what remains unknown and what each experiment is intended to falsify.

## 7. Experiment matrix

| Axis | Initial values |
| --- | --- |
| Workload | F1 general, F1 web-search, F2 streaming; expand to F3/F4 |
| Execution | Open-loop, closed-loop |
| Placement | UE tool execution, cloud tool execution, hybrid |
| Transport | TCP initially; then QUIC, paced UDP |
| Radio | Stable/good, impaired/edge-like when available, movement |
| Concurrency | Single flow; parallel tool bursts; mixed traffic |
| Sessions | One repeatable task; multi-task sequences |

**Key comparison:** For the *same task definition*, compare application intent, actual UE-link traffic and task completion across varied network conditions. Record when closed-loop execution changes the number of steps completed in a fixed duration.

## 8. Non-negotiable engineering invariants

1. Synthetic user data must be distinguishable from genuine application captures.
2. Every log and transfer has stable run/session/transaction/burst identifiers; packet IDs are only assigned where such association is actually possible.
3. Intended application bursts and actual observed transport packets are never conflated.
4. Every reference-derived statistical target retains its source and measurement definition; unverified references remain flagged.
5. Same seed reproduces the intended event process, not necessarily OS/kernel packet timing.
6. Closed-loop dependencies await actual completion conditions rather than an idealized scheduled response.
7. A cloud-only tool exchange cannot be counted as UE traffic.
8. All traffic generation has configurable rates, duration and byte limits, fails closed on interruption, and runs only against authorized endpoints.
9. Timestamp/clock uncertainty accompanies every cross-device latency claim.
10. DSCP markings and network QoS treatment are observed separately.

## 9. Minimal acceptance demonstration

A UE connected over a real mobile network loads a versioned `agentic_websearch` scenario and initiates an LLM-like request. The network-side application returns a response; dependent simulated web/tool operations generate separately classified control and data transfers, some in parallel; the UE emits a follow-up context update and receives a final response. Both endpoints log event IDs and timestamps, packet captures are retained, and an offline report explains traffic by direction and by activity, including sizes, timing, completion, source profile provenance and clock uncertainty. Re-running the scenario with the same seed produces the same intended workload, while actual packet behavior may differ.

## 10. Open decisions

- Android shared-core implementation language/framework, pending build spike.
- Whether packet capture on Android is external, VPN-based, rooted-test-device, or a combination; establish bias per method.
- Exact burst segmentation, statistical P/M/CV definitions and absolute size scales from original TDocs.
- Network availability for comparing QoS behavior; do not assume DSCP-to-5QI mapping.
- Whether agent transactions are scripted workflows or sampled graphs in the first prototype; implement deterministic graphs first.

**Recommendation:** Prioritize the event/provenance contracts, TCP vertical slice and F1 `agentic_websearch` workflow. These expose the hard integration and measurement issues early and preserve the original specification's broader agentic/tool-traffic perspective.

## 11. Status against the current platform (2026-10-20)

The measurement platform in this repository already carries an AI-agent
traffic model: the `app_traffic` flow with profile `agent` (core
`app_traffic/model.rs`, runner `app_traffic/runner.rs`, server object server
in `data-generator`, analysis `app.py`). It covers much of F1; the other
families, the evidence registry and the intended-vs-observed split are open.

### Covered

| Plan item | In the platform |
| --- | --- |
| F1 multi-step LLM exchanges, context growth, compaction | Task of 1–30 LLM calls (lognormal median 6); each call uploads the whole context (4–60 kB start), which grows by the reply and tool results; compaction 600 kB → 60 kB |
| Streamed LLM response | Time to first token (lognormal 0.9 s), then a paced stream at 40–90 tokens/s in 140 B events |
| Tool traffic, parallel calls | MCP calls and web fetches, 1–4 per turn, run in parallel |
| Execution location UE / cloud / hybrid; cloud-only exchanges not UE traffic (invariant 7) | `placement`: cloud tools become a pause in the reply plus the result text, no UE connection |
| Closed-loop progression (invariant 6) | Every step waits for the real transfers to complete |
| Per-activity volume/time attribution | Rows tagged `app_activity` (`llm_call`, `mcp_call`, `web_fetch`, `cloud_tool`, `llm_resume`); per activity wait, think, completion, longest pause, MB up/down |
| TCP over the real OS stack; seeded intent | Yes (invariant 5) |
| Both endpoints log; clock uncertainty (invariant 9) | Dual-clock Parquet logs on mobile and server, offset with uncertainty bounds, kernel receive timestamps |
| Radio information captured separately | `radio.parquet` (Logger), modem queues and radio-log events (`mp-probe`) |
| DSCP observed, not assumed (invariant 10) | `dscp_probe` flow (marking transparency; not yet tied to app traffic) |
| Execution against live mobile data, impaired conditions, movement | Commute runs triggered automatically; head-of-line cycles beside realtime streams |

### Missing

| Plan item | Gap |
| --- | --- |
| §A evidence registry (invariant 4) | All model numbers (TTFT 0.9 s, 250 tokens, 47 B/token, MCP/web-fetch sizes, think time) are engineering estimates without a source. The plan cites ~310 B DL text events plus 54 B TCP ACKs; the platform uses 140 B events. |
| F1 subprofiles | `agentic_websearch` as its own graph (search → results → several page fetches → follow-up inference); `agentic_computer_control` (small frequent commands, screenshot uploads) |
| F2 `chat_llm` | No standalone chat profile (prompt, micro-streamed reply, reading time) |
| F3 `multimodal_visual` | No periodic sizeable UL upload comb (~3.9 s) followed by DL analysis |
| F4 `image_gen`, `video_understanding` | Not modelled |
| Identifiers (invariant 2) | One `object_id` packs agent \| episode \| object; no session / task / step / burst IDs, so analysis cannot aggregate per task or step |
| Intended vs observed (invariant 3) | Socket reads/writes are logged, not the intended event schedule as a separate record |
| Open-loop mode | Only closed loop (the `script` profile runs its actions in sequence) |
| §B versioned scenario with step graphs | Profile + parameters or a linear script; no dependency graph, conditional steps or retries |
| PCAP | No packet capture; TCP is logged per application chunk, so ACKs and on-wire sizes are not visible |
| QUIC, generic paced UDP for app traffic | TCP only |
| Metrics | No UL:DL ratio per family, burst P/M or CV (ratio is straightforward from per-activity MB; P/M and CV need the definitions of §10 first) |
| Distributions and burst shapes | Lognormal only; no Weibull, no periodic comb |
| Per-run report with provenance | Findings page covers app objects and their impact on realtime streams, without provenance |

### Suggested order (consistent with §10)

1. Session / task / step IDs on every app row (the request header already carries an object id) and an intended-event log beside the observed rows.
2. Evidence registry for the profile parameters, starting with the current estimates marked unverified.
3. `agentic_websearch` as a task graph, with an open-loop variant.
4. F2 `chat_llm`, then F3 and F4.
5. Server-side packet capture.
