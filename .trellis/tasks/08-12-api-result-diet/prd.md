# Trim infallible `Result` from DSP hot-path and lifecycle APIs

## Goal

Remove `Result` from public API positions where the `Result` is either never
constructed as `Err`, or where the only `Err` arm encodes a caller programming
error rather than a recoverable runtime condition. Target: reduce per-sample
and per-call ceremony without weakening any genuine validation boundary.

Trigger: an external review claimed "上游更啰嗦, Result 泛滥,
`process_sample -> Result<f64>` 有过度设计嫌疑".

## What I already know (verified against current source, 2026-08-12)

### The review's claim about `process_sample` — CONFIRMED

`src/processor/dsp.rs:337`:

```rust
pub fn process_sample(&mut self, sample: f64, ch: usize) -> Result<f64, ProcessError> {
    if ch >= self.rng_state.len() {
        return Err(ProcessError::InvalidGeometry { ... });
    }
    Ok(self.process_sample_validated(sample, ch))
}
```

- The only `Err` arm is a channel-index bounds check — a caller programming
  error, not a recoverable runtime failure.
- Non-finite *sample* values do NOT error; they are silently absorbed by
  `bypass_or_recover_invalid`. So the module already treats bad data as
  non-fallible; only the index was promoted to `Result`.
- No in-tree caller uses it. `process_validated` (the buffer path) calls
  `process_sample_lipshitz5` / `_tpdf_only` / `_9tap` directly and bypasses it.
- The only call sites in the whole repo are 3 benchmark lines
  (`benches/audio_quality_measurements.rs:1984,1997,2031`), each of which
  immediately does `.expect("channel zero is configured")`.
- Sibling methods on the same type are infallible: `reset`, `set_bits`,
  `set_curve`, `set_enabled`.

### The review's claim about "Result 泛滥" — PARTIALLY TRUE, weaker than stated

Measured from `tests/public-api-all-features.txt` (deduplicated):

| Metric | Value |
|---|---|
| unique `pub fn` | 959 |
| returning `Result` | 216 (22.5%) |
| — process / finish / render | 96 |
| — `other` | 43 |
| — setters | 35 |
| — constructors | 28 |
| — `reset()` | 14 |

22.5% is not "泛滥" for a crate spanning DSP + decoding + HTTP + SQLite.
Constructors, decode, and process paths are legitimately fallible.

### CORRECTION to the initial analysis: `reset() -> Result` is NOT dead weight

An earlier pass claimed all 14 `reset()` implementations are infallible and the
trait signature should be narrowed to `fn reset(&mut self)`. **This is wrong.**
Re-checked all 24 in-tree `fn reset(&mut self) -> Result<...>` bodies:

| Impl | Verdict |
|---|---|
| `src/processor/resampler/mod.rs:1338` | **genuinely fallible** — soxr/rubato `backend.clear()` failure maps to `ProcessError::Backend` |
| `src/processor/dsp_chain.rs:513` | **genuinely fallible** — aggregates child failures via `first_error` |
| 8 impls in `adapters.rs` + `adapters/convolver.rs` + `pipeline.rs:1567` | always `Ok(())` |
| 12 impls under `*/tests.rs` | always `Ok(())` (test doubles) |

Because `StreamingResampler` is a real implementor of the object-safe
`StreamingProcessor` trait and its `reset` really can fail against a native
backend, `trait StreamingProcessor::reset` (`traits.rs:797`) MUST stay fallible.
The always-`Ok` adapters are conforming to a justified trait contract, not
paying for a phantom one. **Recommendation dropped.**

### Setter `finite`-check inconsistency — OPEN, real but low severity

35 public setters return `Result` whose only `Err` source is
`checked_parameter`'s `value.is_finite()` (`src/pipeline.rs:113`). The value is
otherwise *clamped* to a documented domain. So out-of-range is tolerated by
clamping while NaN is rejected to the caller — two different policies on the
same argument.

Counter-evidence that this is deliberate: the archived task
`08-01-unify-parameter-validation-policy` explicitly ratified a **mixed** policy:

> - callback-adjacent infallible setters reject non-finite input and retain the
>   prior value;
> - public fallible setters return `ProcessError::InvalidParameter` before any
>   mutation;
> - finite values clamp only where a documented public domain exists;

So the NaN-rejects / range-clamps split is an intentional, spec-recorded
decision, not an oversight. Reopening it means reversing a prior ADR.

Note `set_eq_band_gain_db`'s band-index rejection is correct and must stay —
`08-01-...` and `07-29-reject-invalid-eq-band-index-at-public-boundaries`
justify it (clamping an index would silently edit a different band).

### BLOCKING CONSTRAINT: the crate is SemVer-frozen

- `Cargo.toml`: `version = "1.0.1"`; published on crates.io (1.0.0 live).
- `CHANGELOG.md`: "Since 1.0.0 the public API is stable: breaking changes are
  reserved for major version bumps."
- CI job `public-api` runs `cargo semver-checks ... --release-type patch`
  against `tests/semver-baseline/{all-features,rubato}/audio_engine_core.json`
  for BOTH feature matrices. Any signature change fails CI.
- `tests/public_api.rs` diffs against `tests/public-api-*.txt` snapshots under a
  pinned nightly (`nightly-2026-07-09`).
- A real downstream consumer exists: `D:/AI/AudioPlayer` depends on this crate
  by git branch and re-exports whole modules. It does not call
  `process_sample`, so that specific change has zero downstream breakage.

Changing `process_sample`'s signature is therefore a **major-version** change
under the project's own stated policy, for a method with 3 in-repo callers and
no known external ones.

## Assumptions (to validate)

- The user wants the smallest correct change, not an API-wide refactor.
- Bumping to 2.0.0 solely to fix one benchmark-only method is not worth it.

## Decision (ADR-lite)

**Context**: `NoiseShaper::process_sample`'s only `Err` arm is a channel-index
programming-error guard. The method has no in-tree caller outside 3 benchmark
lines that immediately `.expect()`. The crate is published at 1.0.x with a
SemVer freeze declared in CHANGELOG and enforced by a
`cargo semver-checks --release-type patch` CI gate.

**Decision** (user, 2026-08-12): Approach B-variant — change the signature to
`-> f64` outright and release it as **1.0.2**, i.e. ship a technically breaking
change under a patch version rather than bumping to 2.0.0.

**Consequences / accepted risks** (explicitly surfaced to the user):
- This violates SemVer and the crate's own CHANGELOG statement ("Since 1.0.0
  the public API is stable: breaking changes are reserved for major version
  bumps"). Any downstream pinned to `^1.0` that calls `process_sample` breaks
  on `cargo update`.
- Measured blast radius is near-zero: crates.io reports 35 total downloads,
  and the one known real consumer (`D:/AI/AudioPlayer`) never calls this
  method. So practical breakage is expected to be nil even though the change
  is formally incompatible.
- If `cargo semver-checks` flags the change under `--release-type patch`, the
  gate must be consciously handled (see Open Questions Q2) — the baseline must
  NOT be silently refreshed to hide a real major violation, per the runbook's
  negative-control policy.

## Open Questions

- **Q2**: if the patch-level semver gate reports a major violation, do we
  (a) keep the gate honest and record an explicit documented exception, or
  (b) relax `--release-type`? To be resolved once the gate is actually run.

## Feasible approaches

**Approach A: additive + deprecate (patch/minor-safe)** — NOT chosen
- Add `NoiseShaper::process_sample_unchecked(&mut self, f64, usize) -> f64`
  (or `process_sample_on(ChannelIndex)`), keep the existing method, mark it
  `#[deprecated(since = "1.1.0", note = "...")]`.
- How: purely additive; `cargo semver-checks --release-type patch` still passes
  (deprecation is not a breaking change); bump to 1.1.0.
- Pros: no downstream break, no major bump, immediate ergonomic win.
- Cons: surface grows rather than shrinks; the wart stays visible until 2.0.

**Approach B: fix properly** — CHOSEN, but shipped as 1.0.2 rather than 2.0.0
- Change `process_sample -> f64` outright, record it in a `2.0.0` section of
  CHANGELOG, regenerate both public-api snapshots + both semver baselines, and
  flip CI's `--release-type` to `major`.
- Pros: the API ends up correct with no legacy alias.
- Cons: forces a 2.0.0 release cadence decision for a very small win; the
  release-gate tasks (`08-01-release-1-0-0`) would need re-running.

**Approach C: document-only**
- Leave the signature; add rustdoc stating the `Err` arm is a programming-error
  guard and that the buffer-oriented `process()` is the intended API.
- Pros: zero risk, zero CI churn.
- Cons: does not address the reviewer's point.

## Requirements (evolving)

- Do not weaken any validation that guards a genuine runtime condition.
- Do not touch `trait StreamingProcessor::reset` — verified genuinely fallible.
- Do not reopen the mixed setter-validation policy without explicit direction.
- Any public-surface change regenerates `tests/public-api-*.txt` AND both
  `tests/semver-baseline/*/audio_engine_core.json` under the pinned nightly.
- Preserve realtime constraints (no alloc/lock/panic on the hot path).

## Acceptance Criteria (evolving)

- [x] `NoiseShaper::process_sample` returns `f64`; channel bounds become a
      documented caller invariant guarded by `debug_assert!`.
- [x] The `ProcessError::InvalidGeometry` construction in that method is gone
      and no longer reachable from it.
- [x] `benches/audio_quality_measurements.rs:1984,1997,2031` updated, no
      `.expect()` on this call.
- [x] `Cargo.toml` version -> `1.0.2`.
- [x] CHANGELOG records the change AND explicitly notes the SemVer exception.
- [x] `cargo test --test public_api` snapshots regenerated for both matrices.
- [x] Both semver baselines regenerated only AFTER the gate result is reviewed.
- [x] Clippy strict + both feature matrices build/test green.
- [x] Hot path unchanged: `process_validated` still bypasses this method, so
      buffer-path performance is untouched.

## Out of Scope (explicit)

- Narrowing `reset()` / `StreamingProcessor` trait signatures — disproved.
- Reworking the 35 setter `finite` checks — prior ADR, needs separate reversal.
- Any change to `set_eq_band_gain_db` index validation.

## Technical Notes

- `src/processor/dsp.rs:337` — subject method.
- `src/processor/traits.rs:797` — trait `reset`, must remain fallible.
- `src/pipeline.rs:113` — `checked_parameter`.
- `.trellis/spec/backend/error-handling.md` — typed-error boundary contract.
- `.trellis/tasks/archive/2026-08/08-01-unify-parameter-validation-policy/` —
  ratified mixed setter policy.
- `.github/workflows/ci.yml:93-136` — public-api + semver gate.


## Outcome (2026-08-12)

Implemented as decided. Verification:

| Gate | Result |
| --- | --- |
| `cargo clippy --all-features --all-targets -D warnings` | clean |
| `cargo clippy --no-default-features --features rubato -D warnings` | clean |
| `cargo test --all-features` | 552 passed, 0 failed |
| `cargo test --no-default-features --features rubato` | 571 passed, 0 failed |
| `cargo doc --all-features --no-deps` | no warnings |
| `cargo semver-checks` both matrices, `--release-type patch` | 223 pass, 31 skip |
| public-api snapshot diff | exactly 4 lines, the intended signature only |

### Q2 resolved: the semver gate is a false negative, not an endorsement

The patch-level gate passes, but that is a **tooling blind spot**, confirmed via
`cargo semver-checks --list`: the only return-type lints cover value -> `()`
and `()` -> value. There is no lint for a general return-type change, so
`Result<f64, E>` -> `f64` is invisible to all 223 checks while still breaking
any caller using `?` / `.expect()` / `match`.

Chosen resolution: **option (a) — keep the gate honest and document the
exception.** The baselines were refreshed (as required for any accepted API
change) but the CHANGELOG explicitly labels the change BREAKING, states that
the green gate does not imply compatibility, and records the migration. The
`--release-type patch` setting was NOT relaxed, since it still provides real
coverage for the 223 detectable classes.

### Release-path note

`1.0.1` was never published to crates.io (only `1.0.0`, 35 total downloads), so
in practice this ships as the first release after 1.0.0. The CHANGELOG keeps
the `1.0.1` section intact, including its "No public API surface changes"
sentence, which remains accurate for that section's own contents.

### Safety verification performed

Removing the `Result` does not create an out-of-bounds path: both
`process_sample_with_taps` and `process_sample_9tap_ring` call
`bypass_or_recover_invalid` first, which short-circuits `ch >= rng_state.len()`
to a pass-through *before* any indexing. Release behavior for an out-of-range
channel is therefore identical to the previous `Err` arm's practical effect
(no state mutation), minus the allocation-free error value.

### Pre-existing issue found (NOT introduced here, NOT fixed here)

`cargo test --test public_api` fails on a clean checkout on this Windows
machine due to line endings: `core.autocrlf=true` with no `.gitattributes`
means the committed snapshots materialize as CRLF while the test regenerates
LF. Verified by stashing all task changes and re-running — it fails
identically. CI runs Ubuntu, so the gate is green there. Left untouched
deliberately: "fixing" it by committing normalized files would rewrite two
large snapshots plus two multi-megabyte baselines for an environment-local
cause. Worth a separate task adding `.gitattributes` with an explicit `eol`
policy for `tests/public-api-*.txt` and `tests/semver-baseline/**`.

### Findings that did NOT become changes

- `StreamingProcessor::reset -> Result` is **justified** and was left alone;
  `StreamingResampler::reset` and `DspChain::reset` genuinely fail.
- The 35 setters' finite-check policy was left alone; it is a ratified ADR from
  `08-01-unify-parameter-validation-policy`, not an oversight.
