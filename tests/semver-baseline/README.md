# Core 2.1 baseline

These three rustdoc JSON files record the reviewed 2.1.0 public contract. They
use `nightly-2026-07-09`, matching the public API snapshot tests. CI keeps
`--release-type minor` so subsequent breaking changes fail. `--release-type
major` skips all compatibility checks and is not a useful gate.

## 2026-11-09 refresh (documented 2.1 equalizer exception)

The 2.1.0 exception is recorded in CHANGELOG and CONTRIBUTING. Before this
refresh, all three matrices were compared against the committed 2.0 baselines
from `83097922d1c5c0da97ce83f84b630a5cf4b3322d` using cargo-semver-checks 0.50.0
and minor policy. Each comparison exited 100: 191 checks passed, five failed,
and 58 were skipped. The five failing categories were:

- `struct_missing`: the four renamed EQ types and their re-exports.
- `inherent_method_missing`: the five `PlaybackParameters` EQ methods renamed
  to identify the IIR path.
- `enum_variant_missing` / `enum_variant_added`: `OutputStageId::Equalizer`
  became `OutputStageId::IirEqualizer`.
- `constructible_struct_adds_field`: `OutputChainParams.eq_kind` is required
  for direct struct literals.

Text review additionally captured the fourth `Arc<AtomicEqKind>` argument on
`IirEqProcessor::new`; the tool reports the old type as missing instead of
comparing constructors across the rename. `PlaybackController` now has
interior mutable FIR control state, reflected by the nightly-only `Freeze`
auto trait changing from positive to negative. Its stable Send/Sync and
unwind-safety bounds remain covered by the snapshots and trait tests.

Each matrix has 201 rows affected by the documented name mapping and 137 net
additional rows. After that mapping, the only removed signatures are the old
three-argument adapter constructor and the `Freeze` implementation. Remaining
additions are the kind selector, its controls and trait implementations, FIR
tap / band-frequency constants, and the required struct field. Default and
all-features snapshots move from 6640 to 6777 rows; local-only moves from 6379
to 6516. No decoder/resampler API changes were introduced by the memory fix.

This refresh establishes the explicitly accepted 2.1 contract; it does not
claim compatibility with 2.0. Old baselines remain at the revision above.
Raw old/new comparisons, hashes and complete row deltas are retained in the
local `11-09-core-210-release` task. Both JSON generations use format 60.

## Core 2.0 history

The initial 2.0 baseline followed the AutoMix extraction into `audio-automix`.

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

## 2026-09-29 refresh (single-source pitch)

Added `analysis::{PitchAnalyzer, PitchConfig, PitchFrame, PitchCandidate}`:
YIN and pYIN stage-1 candidates, raw-window RMS and sample reference positions.
Each text snapshot gains exactly 80 Pitch rows with no removals or other
changes. All three prior baselines passed 196 minor-policy checks (58 skipped)
before this refresh. The same pinned `nightly-2026-07-09` produces JSON format
60. Generation uses `DOCS_RS=1`; the all-features result is API metadata only,
not a native SoXR runtime validation. Runtime acceptance uses default/rubato.
