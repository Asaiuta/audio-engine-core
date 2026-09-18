# Package migration

The unreleased core 2.0 moves AutoMix to `audio-automix 0.1`. Replace imports of
`audio_engine_core::analysis::{analyze_automix, analyze_automix_with_cancel,
AutomixAnalysis, AutomixAnalysisMode, AutomixAnalysisOptions, AutomixError}` with
the corresponding `audio_automix::*` names. Decoder locations, credentials,
cancel tokens, and typed decoder/measurement errors retain their core owners.

There are no legacy re-exports. A core-to-AutoMix dependency would cycle back
through the core. Function signatures, DTO schema 4, mode serialization,
algorithm thresholds, absolute timing, and bounded windows are unchanged.
Core's package version now matches its already documented unreleased 2.0
breaking API cycle; this checkout change does not publish either package.

Only AutoMix's private compute path uses a cancellation predicate; the public
entry still accepts `DecodeCancelToken`. No public backend or PCM trait was added.

## Validation ownership

AutoMix's accuracy runner, unit/integration tests, corpus preparation tools,
and public API snapshots now live under `crates/audio-automix/`. Run corpus
commands from the repository root, with explicit manifest/root arguments.
The converter script bytes and manifest pins remain unchanged by the move.

Core component benchmark schema 2 removes its two AutoMix cases and their
fixture/trial fields. Old component reports are not comparable to schema 2.
The package-local `audio_automix_perf` records Head/Full timing separately with
a declared fixture and complete samples; its fixture/protocol differs from
the old component harness, so old timings are historical only.

## Frozen source-bound research

Historical `.trellis/tasks` experiments read, hash, inject, and sometimes patch
the former `src/analysis/automix.rs` and `automix/tempo.rs`. They are frozen
evidence tied to their recorded revision, not supported against arbitrary HEAD.
This extraction deliberately does not rewrite those scripts or hashes.

For an experiment, use the exact revision and input hashes in its declaration.
For the production source immediately before this extraction, create an isolated
checkout with:

```sh
git worktree add --detach ../audio-engine-core-pre-automix d2816f271d03f2464f01e5a308360f3d02b1ead2
```

This creates source only. Research audio, models, features, and native packages
are separate assets: locate and verify their recorded hashes before replay.
Do not run `cargo clean` on a research artifact directory. Earlier experiments
may require an earlier revision; d2816f2 is not a universal historical baseline.
Untracked experiments must additionally preserve their own script snapshots.

New research should consume the package public API where sufficient. Experiments
that require private instrumentation must pin their source revision and adapter;
the package does not expose private algorithm internals as a compatibility promise.

## Release

Publish core 2.0 before AutoMix 0.1, because AutoMix's manifest declares the core
registry version as well as its local path. Workspace package verification stages
the dependency locally; neither a successful package check nor this migration
publishes a release.
