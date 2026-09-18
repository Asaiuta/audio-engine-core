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
