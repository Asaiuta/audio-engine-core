# audio-automix

Bounded offline music analysis and AutoMix placement evidence. This package
depends on `audio-engine-core` for decoding and loudness/true-peak measurement;
the core has no dependency on AutoMix.

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

From the repository root:

```sh
cargo test -p audio-automix
cargo bench -p audio-automix --bench audio_automix_accuracy -- --quick --enforce
cargo bench -p audio-automix --bench audio_automix_perf -- --quick --out target/automix-perf.json
```

See [accuracy and corpus contracts](docs/automix-accuracy.md),
[retained production/research status](docs/automix-baseline.md), and
[migration and research replay](docs/migration.md).
