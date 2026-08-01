# Maintainability audit research index

This directory is the resumable evidence log for the whole-codebase audit.
An area is marked complete only after its evidence document has been written.

## Status

| Area | Artifact | Status |
|---|---|---|
| Scope, inventory, and validation baseline | `00-scope-inventory-and-baseline.md` | complete at 2026-07-28 14:49 +08:00 |
| Public API and control boundaries | `01-public-api-and-control-boundaries.md` | complete at 2026-07-28 15:01 +08:00 |
| Pipeline and processor-chain boundaries | `02-pipeline-and-chain-boundaries.md` | complete at 2026-07-28 15:13 +08:00 |
| Realtime DSP and analysis modules | `03a-dsp-and-analysis-modules.md` | complete at 2026-07-28 15:53 +08:00 |
| Decoder, source, metadata, diagnostics, and runtime modules | `03b-decoder-and-runtime-modules.md` | complete at 2026-07-28 16:14 +08:00 |
| Resampler architecture and backends | `03c-resampler-modules.md` | complete at 2026-07-28 17:06 +08:00 |
| Legacy surface and duplicated sources of truth | `04-legacy-and-duplication.md` | complete at 2026-07-28 16:45 +08:00 (bench/test duplication sub-scope deferred to area 05) |
| Tests and benchmark maintainability | `05-tests-and-benchmarks.md` | complete at 2026-07-28 18:55 +08:00 |
| Public docs and Trellis spec drift | `06-documentation-and-spec-drift.md` | complete at 2026-07-28 19:32 +08:00 |
| Final ranked synthesis | `99-final-report.md` | complete and quality-verified at 2026-07-28 20:01 +08:00 |
| P1 re-verification and remediation of the unclaimed findings | `07-p1-reverification-and-remediation.md` | complete at 2026-07-30 21:15 +08:00 |
| P2 re-verification and remediation | `08-p2-reverification-and-remediation.md` | complete at 2026-07-31 |

The `07-` and `08-` documents are the only artifacts in this directory that
change source. The audit proper (`00` through `99`) remains read-only evidence;
later user-authorized remediation passes extended the task's scope. Read both
before concluding that any finding in `99-final-report.md` is still open — most
P2 items were already fixed incidentally by the P1 sibling tasks, and `08-`
records which remain.

Current source baseline: `00-scope-inventory-and-baseline.md`. Earlier chat-only
test failures are superseded by the explicit green results recorded there.

## Evidence rules

- Record local time and relevant file mtimes for each snapshot.
- Use exact source/test/spec paths and line numbers where stable.
- Label a finding as correctness defect, boundary debt, maintainability smell,
  documentation drift, or follow-up question.
- Distinguish production use from test/benchmark/example-only use.
- Re-read moving files before finalizing and retain superseded observations as
  such rather than presenting them as current.
