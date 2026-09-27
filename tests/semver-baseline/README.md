# Core 2.0 baseline

These three rustdoc JSON files establish the unreleased 2.0 public contract after
AutoMix moved to `audio-automix`. They use `nightly-2026-07-09`, matching the public
API snapshot tests. CI uses `--release-type minor` so breaking changes still fail.
`--release-type major` skips all compatibility checks and is not a useful gate.

The previous 1.1 baselines remain available at Git revision
`d2816f271d03f2464f01e5a308360f3d02b1ead2`. Comparing those to the extracted core
under minor rules ran 196 checks per matrix: 193 passed, and exactly three checks
reported the six intended removals (`function_missing`, `struct_missing`,
`enum_missing`). All were AutoMix's two functions and four public types in
`audio_engine_core::analysis`. This is a deliberate major-version migration.

Review also confirmed that each text snapshot lost only its 118 AutoMix rows;
all other core rows were unchanged. The new package's snapshots retain the moved
API and auto traits after normalizing the owner and external type defining paths.
The extraction task retains baseline hashes and diagnostic logs under its
`research/` directory. Future refreshes require the same explicit API review;
do not replace baselines merely to make a failing check green.

## 2026-09-26 refresh (L1 completion)

Refreshed all three baselines to the L1 completion surface: absolute raw power
bins and band energy/rise on `DescriptorAnalyzer` (`power_spectrum`, `bands`,
`band_bins`, `BandMeasurements`), the named frozen Slaney mel frontends with
orthonormal MFCCs (`Mel*`, `Mfcc*`), and the streaming spectral HPSS mask
analyzer (`Hpss*`). The review confirmed the change is purely additive — no
removals, no signature changes — and `cargo semver-checks` against the
previous baselines passed all 196 checks per matrix under minor rules before
the refresh. Baselines were regenerated with the same pinned
`nightly-2026-07-09` (rustdoc JSON `format_version` 60 in both old and new).

## 2026-09-27 refresh (MelConfig default removal)

Refreshed all three baselines after removing `impl Default for MelConfig`. The
removal is deliberate per the L1 completion ADR (2026-09-26): callers choose
`MelConfig::neural_frontend()` or `MelConfig::domain_128()` explicitly, so a
frozen frontend is never selected implicitly. The surface is still the
unreleased 2.0 contract. Each text snapshot lost exactly two rows (the
`Default` impl and `MelConfig::default`) and nothing else changed. Before the
refresh, `cargo semver-checks` 0.50.0 against the 2026-09-26 baselines under
minor rules passed all 196 checks per matrix (58 skipped) and reported no lint
for the removal, which was a hand-written rather than derived impl; the snapshot
review is therefore the record of this intentional change. Baselines were
regenerated with the same pinned `nightly-2026-07-09` (rustdoc JSON
`format_version` 60 in both old and new).
