# Public API and Crate Release Hardening

## Goal

Stabilize the public API surface, feature flags, documentation, and examples so the crate is ready for release and accurately reflects the capabilities delivered by the DSP/decoder work, with no overstated claims.

## Requirements

- Review the public surface in `src/lib.rs` and confirm every exported type/function is intentional and documented.
- Audit feature flags (`http`, `loudness-db`, and any added during the roadmap) so each builds independently and is documented in `Cargo.toml` and `README.md`.
- Ensure the crate builds and tests pass under the relevant feature combinations (default, no-default-features, and each feature toggled on individually).
- Verify docs build cleanly with no missing-docs or broken intra-doc links on public items.
- Align `README.md` claims with measured evidence from the quality gates and the completed behavior tasks; keep the true-peak limitation note unless the limiter task proved otherwise.
- Update `CHANGELOG.md` with the roadmap's user-facing changes and any intentional compatibility breaks.
- Confirm `CONTRIBUTING.md` and `NOTICE` are current (license attributions for SoXR, ebur128, SoX-derived coefficients, etc.).
- Validate the examples (`resample_sine`, `equalizer_curve`) compile and run against the final API.
- Run a packaging dry run to catch missing files or metadata before any real publish.

## Acceptance Criteria

- [ ] Every public item in `lib.rs` is documented and intentionally exported.
- [ ] Each feature flag builds in isolation and is documented.
- [ ] `cargo doc` produces no warnings for public items.
- [ ] `README.md` claims are backed by current benchmark/test evidence or an explicit limitation note.
- [ ] `CHANGELOG.md` records user-facing changes and any compatibility breaks for this release.
- [ ] `NOTICE`/attribution files cover all third-party components.
- [ ] All examples compile and run.
- [ ] `cargo publish --dry-run` (or `cargo package`) succeeds.

## Validation Commands

- `cargo build`
- `cargo build --no-default-features`
- `cargo build --no-default-features --features http`
- `cargo build --no-default-features --features loudness-db`
- `cargo test --all-features`
- `cargo doc --no-deps`
- `cargo run --example resample_sine`
- `cargo run --example equalizer_curve`
- `cargo package --allow-dirty`

## Out of Scope

- New DSP/decoder behavior (covered by the sibling capability tasks).
- Performing the actual crates.io publish (this task only proves release readiness).
- UI, device output, or application-integration work outside this crate.
- Strengthening README claims beyond what measured evidence supports.

## Technical Notes

- This task should run after the behavior tasks settle so the public surface reflects actual, measured capabilities.
- Compatibility breaks introduced during the roadmap must be documented explicitly rather than shipped silently.
- Source anchors: `src/lib.rs`, `Cargo.toml`, `README.md`, `CHANGELOG.md`, `CONTRIBUTING.md`, `NOTICE`, `examples/resample_sine.rs`, `examples/equalizer_curve.rs`.
- Shared audit: `../06-12-audio-engine-feature-upgrade/research/current-algorithm-audit.md`.
