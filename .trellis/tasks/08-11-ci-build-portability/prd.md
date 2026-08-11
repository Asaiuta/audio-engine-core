# CI, Build, and Portability Hygiene

2026-08-11 full-code-review follow-up, batch 7 of 8. The engineering review
rated the CI/quality-gate machinery top-decile ("almost no theatrical
steps"); 1.0.1 fixed the packaging leak and the stale README claims. This
task collects the remaining infrastructure debts.

## Goal

Close the gap between CONTRIBUTING's stated verification matrix and what CI
actually runs, make dependency/toolchain rot visible on a schedule instead
of on the next push, fix the one benchmark whose comparison is
methodologically unfair, and remove the known build-script portability
hazards.

## What I Already Know

- **Feature-combination coverage is narrower than CONTRIBUTING requires**:
  CONTRIBUTING:57-62 lists `rubato,http` and `rubato,loudness-db` combos;
  CI runs only all-features and rubato-only endpoints. A missing `#[cfg]`
  fence between optional features can slip through. cargo-hack (powerset or
  curated list) is the standard fix.
- **The lockfree bench's "~50 ns split-atomic" baseline is constructed**
  (`benches/audio_lockfree_params_perf.rs:430-443`): every legacy field
  read goes through `#[inline(never)]` + `black_box` (~30 fields × call
  overhead) — a real naive implementation would inline. The 7 ns snapshot
  and 83 ns ArcSwap numbers are fair. Also single-trial, no warmup/median,
  below the repo's own callback-bench statistical standard. README cites
  the trio; the comparison wording must change with the bench fix.
- **No scheduled CI, no cargo-audit/cargo-deny**: the pinned
  `nightly-2026-07-09` (public-API gate) disappearing, or a dependency
  advisory, only surfaces on the next push. A weekly cron running the
  toolchain probe + audit closes it.
- **vcpkg is the documented preferred Windows path but has zero CI
  coverage** (`docs/installation.md:22-29` vs ci.yml:161-179 which
  provisions MSYS2/pkg-config only): the `vcpkg::find_package` branch of
  build.rs (`:30`) is never exercised.
- **build.rs portability hazards**: `/MACHINE:X64` and `HostX64/x64`
  hardcoded (`build.rs:83`, `:195-198`) — Windows ARM64/x86 MSVC fails;
  a personal-machine convenience probe for `%USERPROFILE%\scoop\apps\msys2`
  ships in the release build script (`:122-138`); the runtime-DLL deployer
  writes outside OUT_DIR into `target/{profile}`, `deps`, `examples`
  (`build/windows_runtime.rs:24-50`) against Cargo's build-script contract,
  with non-atomic `fs::copy` under concurrent builds (self-aware hack,
  `copy_if_changed` mitigates the common path).
- **Cargo.lock policy is contradictory**: `.gitignore:4-6` claims the
  lockfile "is tracked at the workspace root" — it is not tracked at all;
  with rust-cache, CI dependency drift is invisible. Local lock audit found
  all-registry sources, no pins — clean. Decide: track the lockfile (typical
  for a crate with heavy CI gating) or fix the comment and accept floating.
- Design notes recorded, no action forced: `symphonia features=["all"]`
  denies downstream codec trimming; default features carry bundled SQLite +
  reqwest (heavy for a "core" crate); `soxr`+`rubato` co-enabled silently
  prefers soxr (documented, `RESAMPLER_BACKEND_NAME` observable);
  semver-baseline rustdoc JSON (8.9 MB) grows per release (CONTRIBUTING
  already notes the registry-baseline exit).

## Research References

- [`research/review-findings-2026-08-11.md`](research/review-findings-2026-08-11.md)
  — engineering-review problem table rows 4-13 with evidence.

## Requirements

- CI: add the CONTRIBUTING combos via cargo-hack (curated list, not full
  powerset, to bound cost); add a weekly `schedule:` job running MSRV
  check, pinned-nightly availability probe, and cargo-audit (deny optional);
  add one Windows job leg (or matrix include) exercising the vcpkg branch.
- Bench: give the legacy split-atomic baseline a fair inlined
  implementation (keep an `#[inline(never)]` variant only if labeled as
  call-overhead illustration), add warmup + median/p95 per the repo's own
  support library, re-measure, and update the README comparison sentence
  with the new numbers.
- build.rs: derive `/MACHINE:` and the VS host/target directory from the
  target triple; gate or remove the scoop probe; document the OUT_DIR
  contract violation at the deployment site and serialize/atomize the copy
  (write-to-temp + rename).
- Lockfile: make the decision, implement it, and fix the `.gitignore`
  comment either way.
- Semver-baseline growth and default-feature weight: no action this task;
  keep the notes in this PRD as the standing record.

## Out of Scope

- Runner-absolute performance gates (deliberately excluded by design —
  ci.yml:247-249's reasoning stands).
- MSRV/toolchain bumps; releasing.
- Restructuring the benchmark JSON/baseline machinery.

## Technical Notes

- Files: `.github/workflows/ci.yml`, `benches/audio_lockfree_params_perf.rs`,
  `README.md` (comparison sentence), `build.rs`,
  `build/windows_runtime.rs`, `.gitignore`, `CONTRIBUTING.md`.
- The three-valued toolchain probe in `tests/public_api.rs:75-119`
  (Installed/Absent/Indeterminate, panic on indeterminate) is the
  anti-false-green pattern to reuse for any new scheduled probes.
- vcpkg CI leg cost: `cargo-vcpkg` build of libsoxr is slow — cache the
  vcpkg tree keyed on `vcpkg.json`-equivalent metadata.

## Implementation Plan

1. cargo-hack combos + weekly scheduled probe/audit job.
2. Bench fairness rework + README number/wording update.
3. build.rs target-derivation + scoop gate + copy atomicity.
4. Lockfile decision + .gitignore truth.
5. vcpkg CI leg with caching.
