# Bug Analysis: Phase Completion Was Reported As Goal Completion

## 1. Root Cause Category

- **Category**: A/E - Missing Spec and Implicit Assumption
- **Specific Cause**: The original PRD defined a deliberately narrow MVP and
  checked every MVP acceptance item, while the user's broader goal was to cover
  all representative comparable projects. The completion statement followed
  the checked MVP boxes instead of reconciling them with the user's latest
  coverage requirement and the report's own exclusions.

## 2. Why Fixes Failed

1. The first adapter increment was technically valid, but its bounded scope was
   treated as the terminal scope. This was an incomplete-scope failure.
2. The result document was named `final-results.md` even though its scope
   explicitly excluded FFmpeg and SpeexDSP. The filename and passing checklist
   encouraged a stronger conclusion than the evidence supported.
3. Later answers listed deferred projects but did not convert that list into
   blocking acceptance criteria. Visible omissions therefore did not prevent a
   completion claim.

## 3. Prevention Mechanisms

| Priority | Mechanism | Specific Action | Status |
| --- | --- | --- | --- |
| P0 | PRD contract | Give every named representative project one terminal state: `measured`, `not-comparable`, or `infeasible-with-evidence` | DONE |
| P0 | Completion gate | Treat `skipped`, `unavailable`, `deferred`, placeholders, and non-empty exclusions as incomplete coverage | DONE |
| P0 | Evidence wording | Rename/relabel the existing report as phase 1 and prohibit `final`, `all`, or `complete` claims until the matrix closes | DONE |
| P1 | Report integrity | Add a machine-readable coverage matrix and validate terminal state for every required project | DONE |
| P1 | Review checklist | Require docs and PRD exclusions to be reconciled before reporting task completion | DONE |

## 4. Systematic Expansion

- **Similar Issues**: Any benchmark task can pass a local case-set gate while
  omitting a user-requested engine, codec, platform, or workload.
- **Design Improvement**: Separate execution status (`measured`/`skipped`) from
  task-coverage status and validate the required project inventory explicitly.
- **Process Improvement**: Completion review must compare the latest user goal,
  PRD matrix, generated report, and limitations section, not merely count checked
  boxes or passing commands.

## 5. Knowledge Capture

- [x] Expanded this task's PRD and reopened acceptance criteria.
- [x] Added the universal-claim guard to backend quality guidance.
- [x] Added coverage-matrix validation to the benchmark report contract.
- [x] Relabeled the phase-1 result documentation and generated the primary
      representative artifact only after all 11 rows reached terminal state.
