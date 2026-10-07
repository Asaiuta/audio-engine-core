# audio-automix

Bounded offline music analysis and AutoMix placement evidence. This package
depends on `audio-engine-core` for decoding, loudness/true-peak measurement and
opt-in mono descriptors. The core has no dependency on AutoMix.

```rust,no_run
use audio_automix::{analyze_automix, AutomixAnalysisOptions};
use audio_engine_core::decoder::MediaLocation;

# fn main() -> Result<(), audio_automix::AutomixError> {
let result = analyze_automix(
    MediaLocation::local("track.flac"),
    None,
    AutomixAnalysisOptions::default(),
)?;
println!("Tempo evidence: {:?}", result.bpm);
# Ok(())
```

Call on a worker outside the audio callback. `Head` analyzes the bounded head;
`Full` adds the bounded tail and does not decode the intervening track. The
caller owns scheduling, cancellation requests, stale-result handling, queues,
persistence, and playback rendering. The package does not perform two-track mixing.

Default features are `http` and `rubato`; `--no-default-features --features rubato`
provides local-only analysis. `soxr` opts into the core's native backend. At least
one resampler backend is required by the core, even though AutoMix itself does
not resample. SQLite is not enabled by this package. This extraction makes no
compile-time, binary-size, or analysis-speed claim.

The production detector remains v10/P3 with DTO schema 4. Missing tempo/grid
evidence remains absent; confidence is not a calibrated probability. Existing
research models/DLLs are not production dependencies.

Schema 4 also uses sustained sound/silence boundaries and a conservative
bass breakdown/re-entry rule. Drop detection needs at least eight seconds of
complete head evidence, and snaps only to individual usable beats. Vocal
positions are always JSON null and never influence cuts; no validated vocal
detector is currently exposed. These rules have synthetic fixture contracts,
not a real-music accuracy claim.

## Opt-in mono diagnostics

```rust,no_run
use audio_automix::{analyze_automix_with_descriptors, AutomixAnalysisOptions};
use audio_engine_core::decoder::MediaLocation;

# fn main() -> Result<(), audio_automix::AutomixError> {
let result = analyze_automix_with_descriptors(
    MediaLocation::local("track.flac"),
    None,
    AutomixAnalysisOptions::default(),
    None, // optional DecodeCancelToken
)?;
println!("Analysis version: {}", result.analysis.version);
for segment in &result.descriptors.segments {
    println!("{:?}: {}..{}, centroid {:?} Hz ({}/{} frames)",
        segment.role, segment.start_frame, segment.end_frame,
        segment.spectral.centroid_hz.mean,
        segment.spectral.centroid_hz.valid_frame_count,
        segment.spectral.frame_count);
}
# Ok(())
```

This entry point reuses the same bounded decode and returns
`AutomixDescriptorAnalysis { analysis, descriptors }`. Existing entry points,
options and schema 4 fields are unchanged and create no descriptor FFT. The
companion uses schema 1 / `native-mono-l1-v1`: native sample rate, ordered f64
channel averaging, periodic Hann, FFT 4096, hop 1024, flatness power floor
`1e-20` and clipping threshold `1.0`. It reports mono peak/RMS/crest/DC/clipping
and equal-weight centroid/flatness means; RMS does not replace LUFS, and mono
phase cancellation can hide channel peaks.

Every segment exposes planned and actual exclusive frame ends, complete/short
EOF status, nonfinite-input counts and valid spectral denominators. Undefined
measurements are explicit JSON nulls. Head and tail have independent state;
partial FFT tails are not padded. The added loop polls cancellation at most
4096 source frames apart, with at most one descriptor FFT between polls; this
is a work bound, not a wall-clock deadline. Any error fails the whole request.

Compare spectral values only when schema, extractor profile, sample rate,
source channels and configuration all match, with coverage visible. See the
[descriptor migration contract](docs/migration.md#opt-in-descriptor-companion)
for exact fields, null rules and cache requirements. AudioPlayer request/cache/
diagnostic UI integration remains separate pending work; this library entry
does not establish an application consumer or automatic mixing benefit.

From the repository root:

```sh
cargo test -p audio-automix
cargo bench -p audio-automix --bench audio_automix_accuracy -- --quick --enforce
cargo bench -p audio-automix --bench audio_automix_perf -- --quick --out target/automix-perf.json
```

See [accuracy and corpus contracts](docs/automix-accuracy.md),
[retained production/research status](docs/automix-baseline.md), and
[migration and research replay](docs/migration.md).
