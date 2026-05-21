# update_after_review — reconciled documents

> Date: 2026-05-20

Post-review versions of the planning documents after applying **Step 0** of
`../review_remediation_plan.md` (the cheap, no-rework fixes) plus the **M1**
cross-document reconciliation. Originals remain unchanged in `planning/`.

## Files in this folder

- `5_g_field_test_platform_architecture_spec.md` — reconciled spec (M1 applied)
- `design_review.md` — corrected review (C1–C3 applied)

## Changes applied

### `design_review.md` — review self-corrections

- **C1 (finding 2.3):** Removed the unsupported claim that *"the earlier text said
  'separate flow_id'"* — that phrase does not exist anywhere in the spec (verified by
  grep). The recommendation (model bidirectional Realtime UDP as two unidirectional
  flows) is retained; only the false citation was dropped.
- **C2 (finding 2.6):** Re-scoped from *"never defines the detection criteria"* to
  *"underspecified."* §12.7 already gives a partial criterion ("no serving cell for
  >N seconds"); only `N` and the exact mechanism were missing.
- **C3 (finding 2.7):** Removed the dangling reference to a non-existent *"3GPP
  review."* The underlying gap (no clock-calibration protocol) stands on its own.

### `5_g_field_test_platform_architecture_spec.md` — M1 reconciliation (spec was the outlier)

- **`supported_features` removed** from the §5.2 registration request. It duplicated
  the detailed `capabilities` object, which is now the single source.
  AGENTS.md already omitted it.
- **`neighbor_cells` moved** in §12.4 from "Derived fields (post-processing)" to the
  logged primary radio fields — it is directly available via `getAllCellInfo()` and is
  logged at the serving-cell sample rate. AGENTS.md already treated it as logged.
  (`handover_event` remains a derived field.)

## Not changed (already consistent)

- `AGENTS.md`, `CLAUDE.md` — already aligned with the reconciled spec on both points.
- `development_plan.md`, `review_remediation_plan.md` — unaffected by Step 0.

## Remaining work

Steps 1–4 of `../review_remediation_plan.md` (the P0 contracts and everything after)
are not addressed here.

## Note: canonical docs have since advanced

After this snapshot, a **symmetric peer-node** architecture change was made directly to
the canonical living docs (`../5_g_field_test_platform_architecture_spec.md`,
`../../AGENTS.md`, `../../CLAUDE.md`): both nodes act as generator / receiver / loopback /
logger, a test aggregates both nodes' recordings under one `test_id` into `mobile/` and
`server/` subfolders, and GPS + the radio metric trace remain mobile-only singletons (one
per test). The M1 fixes above were also carried into the canonical spec. This folder
remains the review-fix snapshot; the canonical `planning/` docs are the current source of
truth.
