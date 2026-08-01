# Audit codebase maintainability and architectural boundaries

## Goal

Perform a source-backed, read-only audit of the current Rust codebase for
structural debt, including over-design, unclear ownership or abstraction
boundaries, inaccurate naming or contracts, duplicated sources of truth, and
code that is unnecessarily difficult to change or verify. The audit must not
misclassify complexity that is justified by realtime audio, streaming
lifecycle, latency/tail, numerical quality, or allocation-free callback
requirements.

## Requirements

- Cover production Rust code, public APIs, tests, benchmarks, examples, and
  Trellis specifications that make claims about the Rust implementation.
- Keep the audit read-only with respect to source code and existing user work.
- After finishing each review area, immediately persist its evidence and
  conclusion under this task's `research/` directory before moving on.
- Every finding must identify the concrete source location and explain the
  maintenance or correctness consequence; file size alone is not evidence.
- Rank findings by severity and distinguish confirmed defects from design
  smells, documentation drift, and lower-confidence follow-up questions.
- Record validation commands and their exact outcomes. Do not represent a
  started or planned command as completed validation.
- Because the working tree is being edited concurrently, timestamp each
  evidence snapshot and re-read affected files before the final synthesis.
- Explicitly list complex mechanisms that are justified by realtime or DSP
  contracts and therefore should not be labelled as needless complexity.

## Acceptance Criteria

- [x] A timestamped repository and quality-gate baseline is recorded.
- [x] Public API, parameter publication, and control/lifecycle boundaries are
      reviewed and persisted.
- [x] Core pipeline and processor-chain boundaries are reviewed and persisted.
- [x] DSP, decoder, resampler, and utility module boundaries are reviewed and
      persisted.
- [x] Legacy/dead public surface and duplicated configuration or ownership
      models are reviewed and persisted.
- [x] Tests and benchmark harnesses are reviewed for maintainability,
      evidence quality, and failure-localization risks.
- [x] Public documentation and Trellis specs are checked against current
      source contracts.
- [x] The final report ranks findings, separates justified complexity, and
      states the exact snapshot and validation limitations.

## Definition of Done

- Research artifacts are complete enough that a context-compacted session can
  resume without repeating completed review areas.
- The final conclusions are traceable to current source, tests, command output,
  or persisted inspection evidence.
- No source, test, benchmark, task belonging to other work, or existing dirty
  change is modified by this audit.
- The audit task remains unarchived unless the user explicitly asks to archive
  it.

## Technical Approach

Review the repository in bounded domains. Each domain produces one Markdown
artifact with: scope, snapshot, evidence, findings, justified complexity,
quality signals, and remaining cross-checks. Maintain `research/README.md` as
the resumable phase index. Revalidate any file whose timestamp or Git diff
changes after it was inspected.

## Decision (ADR-lite)

**Context**: A single chat-only full-codebase audit is vulnerable to context
compaction and to false conclusions when the dirty working tree changes during
the review.

**Decision**: Persist one evidence document per completed review domain in a
dedicated Trellis task, then synthesize only from those timestamped artifacts.

**Consequences**: The audit is resumable and reviewable, at the cost of more
small documentation files. Findings may be marked superseded when concurrent
source edits invalidate an earlier snapshot; they must not be silently erased.

## Out of Scope

- Implementing fixes or refactors discovered by the audit.
- Rewriting architecture solely to reduce line count or abstraction count.
- Claiming device/driver/DAC or end-to-end latency from library-only evidence.
- Committing, pushing, archiving this task, or changing other active Trellis
  tasks without an explicit user request.

## Remediation Addendum (2026-07-30, user-authorized)

The user directed this task to re-verify its own findings against current source
and then fix them, which supersedes the first "Out of Scope" bullet for a bounded
set. The agreed scope is the seven P1 findings that never received a dedicated
sibling task: #11 build-time drain-policy validation, #12 gapless `--enforce`
false-green, #13 spec feature matrix, #14 quality prose, #15 orphan public effect
configs, #16 crossfeed default divergence, and #17 published clamp ranges.

- P1 #1 to #10 were re-verified as already fixed in the working tree by the
  `07-28`/`07-29` sibling tasks; this task changed nothing for them.
- The user chose deletion over deprecation for the orphan configs in #15.
- P2 boundary debt and P3 themes remain out of scope and unfixed.
- Evidence, judgement per finding, exact changes, validation outcomes, and an
  explicit list of what was deliberately not changed are in
  `research/07-p1-reverification-and-remediation.md`.

### Additional Acceptance Criteria

- [x] P1 #1 to #10 re-verified against current source rather than assumed.
- [x] P1 #11 to #17 re-verified, each classified as refactor, simplification, or
      minimal fix before editing.
- [x] Fixes applied with focused regressions for every behavioral change.
- [x] Both feature matrices, both Clippy matrices, formatting, and diff hygiene
      pass on the resulting tree.
- [x] The gapless `--enforce` false-green is reproduced and shown to fail after
      the fix.
- [x] Findings left open are recorded rather than silently dropped.

## P2 Addendum (2026-07-31, user-authorized)

The user directed this task to continue into the P2 boundary debt under the same
rule: re-verify against current source first, then choose refactor,
simplification, or minimal fix per finding, with destructive change allowed where
warranted and no over-design.

- All twelve P2 findings were re-verified. #1, #3, #4, #5, #10 were substantially
  fixed already by the P1 sibling tasks; #2, #9, #11 partly.
- Re-verifying #4 surfaced a defect above its P2 rank: `energy_profile` was
  allocated from an unbounded container-declared duration, reachable from the
  public `analyze_automix` with an ordinary untrusted file. It was fixed here.
- Fixed this pass: the #4 allocation bound, #3 non-finite standalone setters,
  #6 decoder metadata as observation only, #7 loudness-cache freshness, the #2
  controller proxy trio, the #5 cancel-token protocol, and #12 spec ownership
  (including `pipeline.rs` being listed as a logging-permitted file after it
  became the realtime callback host).
- Left open and recorded with reasons: #8 resampler facade, #9 legacy lifecycle
  policy, #4's `Result<_, String>`, #1/#2/#10/#11 residuals, and all P3 themes.
- Evidence, per-finding judgement, one corrected judgement, exact changes,
  validation outcomes, validation limits, and what was deliberately not changed
  are in `research/08-p2-reverification-and-remediation.md`.

### Additional Acceptance Criteria

- [x] All twelve P2 findings re-verified against current source rather than
      assumed from the report.
- [x] Each remediated finding classified as refactor, simplification, or minimal
      fix before editing.
- [x] Fixes applied with focused regressions for every behavioral change.
- [x] Both feature matrices, both Clippy matrices, formatting, and diff hygiene
      pass on the resulting tree.
- [x] Breaking API changes recorded in `CHANGELOG.md` and reflected in `README.md`.
- [x] Contracts changed by these fixes are owned by a Trellis spec rather than
      only by rustdoc.
- [x] Findings left open are recorded rather than silently dropped.

## Technical Notes

- Repository: `D:\AI\audio-engine-core`
- Initial branch: `main`
- The initial working tree already contained source changes and unrelated
  untracked files; all are user/other-process work and must be preserved.
- The backend spec index identifies realtime safety as the primary invariant.
