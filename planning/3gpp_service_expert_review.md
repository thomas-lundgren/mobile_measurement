# 3GPP Service Expert Review — 5G Field Test Platform

> Reviewer perspective: 3GPP systems engineer, 5G QoS & service assurance background.
> Use case: Daily commute data collection to build a longitudinal database for jitter/latency analysis.

---

## Constraints Update (Post-Review)

The following hard constraints were established after initial review:

| Constraint | Implication |
|---|---|
| **Reference device: Google Pixel 9 Pro** | Only standard Android Telephony APIs available. No OEM-specific diagnostics, no root access. |
| **UE forced to RRC_CONNECTED active state** | DRX is explicitly out of scope. No DRX-induced jitter expected during tests. |
| **No 5QI/QFI data available** | QoS flow context cannot be logged. PDB compliance tracking is impossible. |
| **No RRC state transitions available** | Cannot log `RRC_IDLE`/`RRC_INACTIVE` entries/exits. |
| **Handovers derived retrospectively** | No explicit HO events from Android APIs. PCI/NCI deltas in `radio.parquet` used instead. |

These constraints significantly narrow the actionable recommendations. The review below is preserved in full, with **constraint-aware annotations** marking items that are infeasible on the Pixel 9 Pro.

---

## Executive Summary

The platform has a solid foundation for packet-level timing analysis. The dual-timestamp model, local-first logging, and capability-tolerant design are architecturally sound. However, from a 3GPP service perspective, the architecture currently lacks the **control-plane and QoS-context bindings** needed to explain *why* jitter and latency behave the way they do. The spec treats the network as a "black pipe." For commute-based longitudinal analysis, you need to correlate every packet delay spike with the 3GPP state changes that caused it.

**Verdict:** Ready for MVP, but several 3GPP-aligned enhancements are strongly recommended before the database becomes analytically useful at scale.

---

## 1. Strengths (Keep These)

| Feature | Why It Matters for 3GPP Analysis |
|---|---|
| Dual timestamp (`t_mono_ns` + `t_utc_ns`) | Essential for correlating UE-side radio events with server-side observations. Monotonic for intra-UE sequencing; UTC for cross-domain alignment with network traces. |
| Local-first logging | Matches 3GPP trace collection principles (TS 32.423). Never lose data due to coverage gaps — critical on a commute. |
| Inter-packet gap logging | Directly measures **packet delay variation (PDV)**, the 3GPP-relevant metric for jitter-sensitive services (VoNR, gaming). |
| `expected_interval_ns` vs `actual_interval_ns` | Enables detection of **network-induced packet clustering** — a key indicator of scheduler backlog or DRX misalignment. |
| GPS correlation | Allows mapping latency/jitter to **geographic cells and route segments**. Essential for longitudinal "same place, different day" analysis. |
| Capability-tolerant radio logging | Correctly acknowledges that not all devices expose the same 3GPP API surfaces (e.g., some OEMs hide NR layer-3 messages). |

---

## 2. Critical Gaps

### 2.1 No QoS Flow Context (5G SA) — *INFEASIBLE on Pixel 9 Pro*

**Problem:** The platform logs packets and radio metrics, but there is **no linkage to the 5G QoS Flow** carrying the traffic. In 5G SA, the UPF maps traffic to a QoS Flow via Packet Detection Rules (PDRs), and the gNB schedules each QoS Flow according to its 5QI characteristics.

**What you cannot explain without this:**
- Why did latency spike? Was the QoS Flow's PDB (Packet Delay Budget) exceeded, or was the flow remapped to a different 5QI?
- Did the network downgrade the flow from GBR to Non-GBR during congestion?
- Are you measuring the "default" QoS flow (5QI=9, best effort) while comparing against VoNR expectations (5QI=1, PDB=100ms)?

**3GPP References:** TS 23.501 §5.7, TS 28.554 §5.2

**Recommended addition (if APIs were available):**

```json
{
  "qos_flow_id": 5,
  "5qi": 1,
  "pdb_ms": 100,
  "priority_level": 20,
  "gbr_bps": 20000,
  "mbr_bps": 20000,
  "qfi_changed": false,
  "slice_info": {
    "sst": 1,
    "sd": "000001"
  }
}
```

**Pixel 9 Pro reality:** `TelephonyManager` does **not** expose 5QI, QFI, or QoS flow information via public APIs. This is **out of scope** for the MVP. Document as a known limitation.

---

### 2.2 No Mobility Event Logging — *DERIVED from radio log on Pixel 9 Pro*

**Problem:** The `events.parquet` file lists generic events (`TEST_START`, `TEST_STOP`, `ERROR`) but has **no 3GPP mobility events**. On a commute, handovers and cell changes are the dominant causes of jitter and latency spikes.

**What you cannot explain without this:**
- Was the 50ms jitter spike caused by a handover, a scheduling delay, or cross-traffic?
- Did the UE drop to `RRC_INACTIVE` at a traffic light, causing a 200ms resumption latency on the next packet?
- Was there an SN change (Secondary Node change in NSA) or an intra-gNB beam switch?

**Pixel 9 Pro reality:**
- **Explicit HO events** are **not** available via `PhoneStateListener` or `TelephonyCallback`.
- **PCI/NCI changes** in `CellInfo` updates **are** available and can be sampled at high frequency.
- **Recommendation:** Log `CellInfo` at 1 Hz (or faster). Derive handover events in post-processing by detecting PCI/NCI changes between consecutive `radio.parquet` samples.

**Recommended events for `events.parquet`:**

| Event | Source | Typical Latency Impact |
|---|---|---|
| `HANDOVER` | Derived from PCI/NCI delta in `radio.parquet` | 20–200 ms |
| `SIGNAL_LOSS_START` | Explicit (no serving cell for >N seconds) | N/A |
| `SIGNAL_LOSS_END` | Explicit (serving cell re-acquired) | N/A |
| `COVERAGE_GAP` | Derived from RSRP < threshold in `radio.parquet` | Variable |
| `BAND_CHANGE` | Derived from NRARFCN delta in `radio.parquet` | 10–100 ms |
| `NEIGHBOR_COUNT_CHANGE` | Derived from neighbor cell list delta | Indicator of approaching cell edge |

**Items NOT available on Pixel 9 Pro:**
- `RRC_STATE_CHANGE` — no public API
- `BEAM_SWITCH` — no public API
- `BEARER_MODIFICATION` — no public API
- `SECONDARY_NODE_ADD/RELEASE` — no public API

---

### 2.3 No DRX / C-DRX Logging — *OUT OF SCOPE (UE forced active)*

**Problem (original):** Connected-mode DRX (C-DRX) is one of the **largest contributors to downlink jitter** in 5G. When the UE is in a DRX OFF period, downlink packets are buffered at the gNB until the next ON period.

**Pixel 9 Pro reality:** The UE is forced to **RRC_CONNECTED active state** during tests. DRX is explicitly **out of scope**. This is a valid simplification for the MVP — jitter measured during active state is dominated by scheduling and mobility, not DRX quantization.

**Recommendation:** Document this assumption clearly. Any jitter observed during tests can be attributed to:
1. Handover / cell reselection
2. Scheduling delay (gNB buffer, competing UEs)
3. Radio link degradation (BLER, poor SINR)
4. Core network delay (UPF processing, transport)

Not DRX.

---

### 2.4 `radio.parquet` is Insufficient for Service-Level Analysis — *PARTIALLY ADDRESSABLE*

**Current fields:** PCI, NCI, TAC, NRARFCN, RSRP, RSRQ, SINR, connection state.

**Available on Pixel 9 Pro via standard APIs:**

| Field | Android API | Status |
|---|---|---|
| `pci` | `CellIdentityNr.getPci()` | ✅ Available |
| `nci` | `CellIdentityNr.getNci()` | ✅ Available |
| `tac` | `CellIdentityNr.getTac()` | ✅ Available |
| `nrarfcn` | `CellIdentityNr.getNrarfcn()` | ✅ Available |
| `rsrp` | `CellSignalStrengthNr.getSsRsrp()` | ✅ Available |
| `rsrq` | `CellSignalStrengthNr.getSsRsrq()` | ✅ Available |
| `sinr` | `CellSignalStrengthNr.getSsSinr()` | ✅ Available |
| `earfcn` / `lte_pci` / `lte_rsrp` | `CellInfoLte` (if NSA) | ✅ Available |
| `neighbor_cells` | `TelephonyManager.getAllCellInfo()` | ✅ Available |
| `bandwidth_mhz` | `CellIdentityNr.getBandwidth()` | ⚠️ Best effort (API 30+) |

**NOT available on Pixel 9 Pro without root:**

| Field | Why It Matters | Status |
|---|---|---|
| `cqi` | Low CQI → conservative MCS → lower spectral efficiency | ❌ Not available |
| `mcs` | Modulation and coding scheme | ❌ Not available |
| `bler` | High BLER triggers retransmissions (+8–32ms each) | ❌ Not available |
| `harq_retransmissions` | Direct measure of link reliability | ❌ Not available |
| `mimo_layers` | Fewer layers = lower throughput | ❌ Not available |
| `tx_power_dbm` | High TX power = poor UL coverage | ❌ Not available |
| `bwp_id` | Active BWP determines resources | ❌ Not available |
| `ca_band_combination` | CA status | ❌ Not available |

**Recommendation for MVP:**
- Log everything that **is** available (serving cell + neighbors) at high frequency.
- Add a **capability tier** system: Tier 1 = always on Pixel 9 Pro, Tier 2 = best effort, Tier 3 = root/OEM only.
- Use **RSRP/RSRQ/SINR trends** as proxies for link quality. A rapid RSRQ drop often correlates with BLER increase even if BLER itself is not logged.

---

### 2.5 No Packet Delay Budget (PDB) Compliance Tracking — *INFEASIBLE on Pixel 9 Pro*

**Problem:** 3GPP defines standardized PDB values per 5QI. The platform measures delay but does not compare against the **contractual delay budget**.

**Pixel 9 Pro reality:** Without 5QI/QFI visibility, PDB compliance tracking is **impossible**.

**Workaround:**
- Document that all tests run on the **default bearer** (5QI=9, PDB=300ms) unless the operator has configured a dedicated QoS flow.
- Use **300ms** as the implicit PDB for best-effort traffic.
- Flag packets exceeding 300ms as "likely PDB violations" with the caveat that the actual PDB is unknown.

---

### 2.6 No Throughput/QoS Saturation Detection

**Problem:** The platform generates traffic at fixed rates but does not verify whether the network is actually delivering the configured rate. If the UDP probe sends at 1 Mbps but the QoS flow's MBR is 128 kbps, the excess traffic is either dropped or demoted to best-effort.

**Pixel 9 Pro reality:** This is **addressable**. The client knows exactly how much traffic it sent and received. The server knows exactly how much it sent and received. Achieved throughput can be computed from `packets.parquet`.

**Recommendation:**
- Add **achieved throughput** per flow to `summary.json`.
- Add a `FLOW_RATE_MISMATCH` derived event if achieved rate deviates >20% from configured rate.
- For Bulk TCP, log **TCP throughput** from the socket layer if possible (via `TrafficStats` or per-socket counters on Android).

---

### 2.7 No Uplink Interference / LBT Context (NR-U / n77-n79) — *FUTURE*

**Problem:** If testing includes NR-U (unlicensed spectrum, e.g., n77, n79), uplink latency is heavily affected by LBT failures.

**Pixel 9 Pro reality:** The Pixel 9 Pro supports n77/n78/n79. NR-U deployment info is **not** available via APIs. Mark as **future**.

---

### 2.8 Time Synchronization is Inadequate for True OWD — *ADDRESSABLE*

**Problem:** The current OWD formula assumes client UTC and server UTC are synchronized. Android NTP sync can drift by ±50ms between reboots.

**For longitudinal analysis:** If the UE reboots between commutes, the clock offset changes, making day-to-day OWD comparison invalid.

**3GPP-aligned recommendations (all feasible on Pixel 9 Pro):**

1. **Use GPS time as the common reference** where available:
   - Android `Location.getTime()` returns UTC derived from GPS.
   - This is more stable than NTP-derived UTC on the client.
   - Add `t_gps_ns` as a third timestamp domain in `clock.parquet`.

2. **Implement a per-session clock calibration protocol:**
   - At test start, exchange a burst of UDP probe packets.
   - Compute minimum RTT/2 as the initial offset estimate.
   - Re-calibrate periodically during the test.
   - Log calibration snapshots to `clock.parquet`.

3. **Report OWD with uncertainty bounds:**
   ```json
   {
     "owd_ul_ms": 12.3,
     "owd_ul_uncertainty_ms": 5.0,
     "owd_dl_ms": 8.7,
     "owd_dl_uncertainty_ms": 5.0
   }
   ```

**Implementation priority: P1.** This is the single most important fix for making the longitudinal database trustworthy.

---

### 2.9 No Network Slice (S-NSSAI) Awareness — *INFEASIBLE on Pixel 9 Pro*

**Problem:** 5G introduces network slicing. A commute may traverse areas served by different slices.

**Pixel 9 Pro reality:** S-NSSAI is **not** exposed via public Android APIs. Mark as **known limitation**.

---

## 3. Recommendations for the Commute Use Case

### 3.1 Session Naming & Route Matching

For longitudinal analysis, sessions must be **route-identifiable**:

```text
session_2026_05_20/
  commute_home_to_work_001/   ← route tag
    header.json               ← includes route_id, direction
    ...
```

**Add to `header.json`:**
- `route_id`: user-defined route identifier (e.g., "home_to_work")
- `route_direction`: "A_to_B" or "B_to_A"
- `route_fingerprint`: hash of GPS waypoints for automatic route matching

### 3.2 Per-Route Summary Statistics

The current `summary.json` is per-test. For commute analysis, add a **per-route rollup** (computed server-side or in post-processing):

```json
{
  "route_id": "home_to_work",
  "session_count": 47,
  "owd_ul_p50_ms": 14.2,
  "owd_ul_p99_ms": 78.5,
  "owd_dl_p50_ms": 10.1,
  "owd_dl_p99_ms": 55.3,
  "jitter_ms": 3.2,
  "handover_count": 7,
  "worst_cell_pci": 123,
  "worst_cell_location": { "lat": 59.3, "lon": 18.1 }
}
```

### 3.3 Event-to-Packet Cross-Reference

Every packet in `packets.parquet` should be joinable to the nearest preceding mobility event. Add an `event_id` or ensure both files share `t_mono_ns` for range-joins.

### 3.4 Handle Commute Interruptions Gracefully

Commutes are not continuous lab tests. Add explicit events for:
- `SIGNAL_LOSS_START` / `SIGNAL_LOSS_END`
- `APP_BACKGROUNDED` / `APP_FOREGROUNDED`
- `DOZE_ENTER` / `DOZE_EXIT` (Android battery optimization)

---

## 4. Data Model Recommendations

### 4.1 Expanded `radio.parquet` (Pixel 9 Pro Reality)

```text
radio.parquet
  - t_mono_ns
  - t_utc_ns
  - pci
  - nci
  - tac
  - nrarfcn
  - rsrp
  - rsrq
  - sinr
  - earfcn           (if NSA)
  - lte_pci          (if NSA)
  - lte_rsrp         (if NSA)
  - bandwidth_mhz    (best effort)
  - cell_connection_status
  - neighbor_cells   [{pci, rsrp, rsrq}]
```

**Derived in post-processing:**
- `handover_event` — true if PCI or NCI changed since previous sample
- `band_change` — true if NRARFCN changed
- `coverage_gap` — true if RSRP < -120 dBm

### 4.2 `events.parquet` (Pixel 9 Pro Reality)

**Logged explicitly:**
```text
TEST_START, TEST_STOP
CLIENT_CONNECTED, SERVER_DISCONNECTED
SIGNAL_LOSS_START, SIGNAL_LOSS_END
APP_BACKGROUNDED, APP_FOREGROUNDED
DOZE_ENTER, DOZE_EXIT
FLOW_RATE_MISMATCH
ERROR
```

**Derived in post-processing from `radio.parquet`:**
```text
HANDOVER
BAND_CHANGE
COVERAGE_GAP
NEIGHBOR_COUNT_CHANGE
```

### 4.3 `summary.json` Enhancements

```json
{
  "rtt_percentiles": { "p50": 22.5, "p95": 55.0, "p99": 120.0 },
  "owd_ul_percentiles": { "p50": 14.2, "p95": 45.0, "p99": 85.0 },
  "owd_dl_percentiles": { "p50": 10.1, "p95": 35.0, "p99": 65.0 },
  "owd_uncertainty_ms": 5.0,
  "loss_percent": 0.12,
  "jitter_ms": 3.2,
  "inter_packet_stats": { "mean_gap_deviation_ms": 1.8, "burst_count": 12 },
  "handover_count": 7,
  "unique_cells_seen": 12,
  "neighbor_stats": { "max_concurrent": 8, "mean_count": 4.2 },
  "radio_stats": { "rsrp_min": -105, "rsrp_mean": -82, "sinr_min": 3 },
  "gps_start": { "lat": 59.3, "lon": 18.0 },
  "gps_end": { "lat": 59.4, "lon": 18.1 },
  "pci_changes": 7,
  "upload_state": "COMPLETE"
}
```

---

## 5. Traffic Model Recommendations for Commute Analysis

### 5.1 The UDP Probe Rate is Under-Specified

**Current spec:** No probe rate specified.

**3GPP recommendation:**
- **VoNR-like:** 50 packets/second (20ms interval) to match AMR-WB frame timing.
- **Gaming-like:** 100 packets/second (10ms interval) to match interactive packet bursts.
- **Adaptive:** Probe rate should be configurable per test plan.

**Why:** Jitter statistics are meaningless without knowing the sampling rate. A 1 pps probe cannot detect 20ms scheduling jitter.

### 5.2 Add a "Background Load" Flow

**Problem:** Real-world commutes involve background app traffic. The platform currently tests in a "clean" environment.

**Recommendation:** Add an optional background UDP/TCP flow that generates low-rate cross-traffic. This allows measuring scheduling fairness and whether the network correctly prioritizes traffic if QoS flows are in use.

---

## 6. Implementation Priority for MVP (Pixel 9 Pro Constraints)

| Priority | Item | Rationale | Feasibility |
|---|---|---|---|
| **P0 (MVP)** | Keep existing packet timing, GPS, dual-timestamp model | Core value proposition | ✅ Pixel 9 Pro |
| **P0 (MVP)** | Log serving cell + neighbors at 1 Hz | Source for derived HO events | ✅ Pixel 9 Pro |
| **P1** | GPS-time clock calibration | Makes OWD trustworthy across days | ✅ Pixel 9 Pro |
| **P1** | Add `FLOW_RATE_MISMATCH` detection | Detects network saturation | ✅ Computed from logs |
| **P1** | Add `SIGNAL_LOSS_*`, `DOZE_*`, `APP_*` events | Commute-specific interruptions | ✅ Pixel 9 Pro |
| **P2** | Per-route rollup summaries | Enables longitudinal queries | ✅ Server-side |
| **P2** | Background cross-traffic flow | Measures scheduling fairness | ✅ Both endpoints |
| **—** | DRX logging | UE forced active | ❌ Out of scope |
| **—** | 5QI/QFI logging | Not exposed on Android | ❌ Infeasible |
| **—** | RRC state transitions | Not exposed on Android | ❌ Infeasible |
| **—** | CQI/MCS/BLER/HARQ | Not exposed on Android | ❌ Infeasible |
| **—** | S-NSSAI awareness | Not exposed on Android | ❌ Infeasible |

---

## 7. Final Assessment

| Criterion | Score | Notes |
|---|---|---|
| Packet-level timing accuracy | ★★★★★ | Dual timestamps, local logging — excellent |
| Radio-layer context | ★★★☆☆ | Basic RSRP/RSRQ/SINR + neighbors available; CQI/MCS/BLER/HARQ unavailable |
| Mobility awareness | ★★★☆☆ | Handovers derivable from PCI/NCI deltas; explicit HO events unavailable |
| QoS context | ★☆☆☆☆ | 5QI/QFI/PDB completely unavailable on Pixel 9 Pro — known limitation |
| Longitudinal analysis support | ★★★☆☆ | Needs GPS-time calibration for day-to-day OWD comparison |
| 3GPP KPI alignment | ★★☆☆☆ | Custom metric names; no PDB compliance tracking possible |

**Bottom line:** With the Pixel 9 Pro constraints, the platform can build a **high-quality longitudinal database of packet timing and cell-level radio metrics**. The main analytical power comes from:
1. Correlating jitter spikes with **derived handover events** (PCI/NCI changes).
2. Mapping latency to **geographic cell coverage** (GPS + PCI).
3. Tracking **trends over time** on the same route (same place, different day, different network load).

What you **lose** with these constraints is the ability to answer "which QoS flow was this on?" and "what was the DRX state?" — but those are acceptable trade-offs for a practical, non-rooted device.

---

*Review completed. Recommend incorporating P0 and P1 items into the MVP architecture spec before implementation begins.*
