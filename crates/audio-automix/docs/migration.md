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

## Opt-in descriptor companion

The additive entry point is:

```rust,ignore
pub fn analyze_automix_with_descriptors(
    location: MediaLocation,
    credentials: Option<HttpCredentials>,
    options: AutomixAnalysisOptions,
    cancel_token: Option<DecodeCancelToken>,
) -> Result<AutomixDescriptorAnalysis, AutomixError>;
```

It returns `{ analysis, descriptors }` from one bounded decode. `analysis` is
the existing `AutomixAnalysis` with version 4. The original two entry points,
`AutomixAnalysisOptions`, legacy JSON fields and their computation remain
unchanged; they allocate no descriptor analyzer and run no descriptor FFT.
Descriptors never feed tempo, loudness matching, cuts, EQ or transition length.

All report types are exported from `audio_automix`: `AutomixDescriptorAnalysis`,
`DescriptorReport`, `DescriptorReportConfig`, `DescriptorSegment`,
`DescriptorSegmentRole`, `DescriptorCoverageStatus`, `DescriptorSignal`,
`DescriptorSpectralSummary` and `DescriptorMetricSummary`. They are producer
reports with `Serialize`; application-side parsing and version validation remain
the consumer's responsibility.

### Wire identity and fields

Every key is present. Unavailable measurements serialize as JSON `null`, never
as omitted keys or zero substitutes. Finite values are not rounded before
serialization.

| Object | Fields |
| --- | --- |
| Report | `schema_version = 1`, `extractor_profile = "native-mono-l1-v1"`, `sample_rate_hz`, `source_channels`, `config`, `segments` |
| Config | `channel_policy = "arithmetic_mean_f64"`, `fft_size = 4096`, `hop_size = 1024`, `window = "hann_periodic"`, `flatness_power_floor = 1e-20`, `clipping_threshold = 1.0` |
| Segment | `role` (`head`/`tail`), `start_frame`, `planned_end_frame`, `end_frame`, `coverage_status` (`complete`/`short_eof`), `nonfinite_input_frame_count`, `mono_signal`, `spectral` |
| Mono signal | `sample_count`, `sample_peak`, `rms`, `crest_factor`, `dc_offset`, `clipping_count`, `clipping_ratio` |
| Spectral | `frame_count`, `nonfinite_frame_count`, `last_end_frame`, `centroid_hz`, `flatness` |
| Each spectral metric | `mean`, `valid_frame_count` |

`sample_peak`, RMS and signed DC use linear amplitude units; crest is linear
peak/RMS. Clipping counts `abs(mono) >= 1` and its ratio is count/sample_count.
This describes the arithmetic-mean mono signal, not channel peaks or true peak:
opposite channels can cancel. RMS is unweighted and does not replace LUFS.
Centroid is in Hz; flatness and clipping ratio are dimensionless in 0..=1.

The source is not resampled. Channels are summed in source order in f64 and
then divided by channel count. Each FFT frame has its unwindowed mean removed
before periodic Hann; power is `|FFT/N|^2` over bins `1..=N/2` without doubling.
The profile also pins core rolloff to `0.85`, contrast and power-band edges to
the empty/default octave geometry, and band-rise floor to `1e-10`; those
measurements are not report fields and do not add configurable request options.

### Coverage, missing values and cancellation

Windows reuse the existing normalized cap (default 60 s, clamped to 5..300 s;
nonfinite caps use the default) and integer-frame plan. With known length T and
window W, head is `[0, min(T,W))`; Full adds tail
`[max(head.end,T-W), T)` only when T exceeds head.end. Unknown length has only
head `[0,W)`. No data is reported for an undecoded gap. Each segment starts fresh
even when head and tail are adjacent.

Frame positions are absolute and ends are exclusive. Actual end is
`start_frame + mono_signal.sample_count`; it never substitutes metadata for
observed PCM. A planned but empty segment is retained. `complete` means actual
end equals planned end; `short_eof` means EOF arrived earlier. A seek beyond the
planned tail origin still fails; an early seek skips to that origin.

For segment length L, full spectral count is zero below N=4096, otherwise
`1 + floor((L-N)/1024)`. The last end is `start + N + (count-1)*1024`, or null
with no complete window. Partial tails count only toward signal. Each finite,
present metric enters the ordered recurrence
`mean += (value - mean) / valid_frame_count`; no frames are skipped because a
decoder packet crosses multiple hops.

Empty signal measurements are null with sample_count zero. Nonempty silence
has zero peak/RMS/DC/clipping and null crest. Silence/DC spectral windows count
in frame_count but contribute no metric values. A source frame containing any
nonfinite channel or nonfinite downmix counts once as polluted. It makes that
segment's signal measurements null; spectral windows recover after the polluted
frame leaves their window. Polluted windows count in both frame_count and
nonfinite_frame_count. A metric with zero valid frames has mean null.

`AutomixError::Descriptor(ProcessError)` retains the typed core source for
descriptor construction/input-validation errors. `From<ProcessError>` still
maps to the existing Loudness variant; existing loudness rejection is not
swallowed to produce diagnostics. Cancellation, decode and seek errors fail the
whole public request. The descriptor loop polls before input and after at most
4096 source frames and one FFT; no wall-clock cancellation deadline is promised.

### Consumer and release boundaries

Spectral comparison requires exact equality of schema, profile, sample rate,
source channels and config; keep window coverage and valid/total counts visible.
Reject unknown schema/profile and nonfinite or unsafe-integer wire values.
Current u64 positions/counts stay within JavaScript safe integers under the
existing 24-hour source-duration and u32 sample-rate limits.

Application caches must separate opt-in state, schema/profile/config, exact
normalized window identity, and producer revision in addition to source/file/
mode identity. Rounded milliseconds are not a sufficient window key. Producer
updates invalidate cached results even when the wire schema is unchanged.
Keep descriptor schema, extractor identity, crate SemVer and analysis version
independent; changing formulas or numerical aggregation requires a new profile.

AudioPlayer dependency migration, opt-in request, runtime DTO checks, cache
integration and diagnostic UI are pending separate work. This library addition
does not complete that product path. Release verification covers the package's
serial feature/API/SemVer gates, complete available-corpus legacy parity and
measured opt-in/off costs. The repository's `docs/quality.md` records the dated
integration evidence and its limits.

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
