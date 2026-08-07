# Directory Structure

> The actual layout of this Rust audio-core crate. Reflects the live `src/`
> tree; update it when the module layout changes.

---

## Crate Role

`audio-engine-core` is an app-agnostic library of decoder, DSP, loudness,
resampling, and streaming-pipeline primitives, extracted from the Lyne audio
engine. It owns no audio device, no server routes, and no UI; the consuming
application layers those on top. There is no `main.rs` and no "business logic"
layer — every module is a reusable primitive.

## Top-Level Layout

```
src/
├── lib.rs            # crate root: module decls + curated public re-exports
├── channel_layout.rs # ChannelLayout, ChannelPosition (positional channel roles)
├── config.rs         # LoudnessConfig, NormalizationMode
├── decoder.rs        # decoder module root (StreamingDecoder)
├── diagnostics.rs    # diagnostic helpers
├── pipeline.rs       # AudioPipeline / ring-buffer streaming primitive
├── runtime.rs        # runtime helpers
├── decoder/
│   ├── error.rs      # DecoderError, NetworkError, DecodeCancelToken
│   ├── source.rs     # input sources (local file, optional http)
│   ├── source/http_policy.rs # HTTP DNS, IP-literal, and redirect address policy
│   ├── streaming.rs  # StreamingDecoder: probe, decode_next(_into), seek, gapless trim
│   ├── metadata.rs   # stream info / metadata extraction
│   └── tests.rs      # decoder unit tests
└── processor/
    ├── mod.rs        # processor module root + public re-exports
    ├── dsp.rs        # db<->linear, VolumeController, NoiseShaper
    ├── eq.rs         # 10-band IIR (BiquadSection, Equalizer)
    ├── fir_eq.rs     # FIR EQ design (FirEq, FirPhaseMode, STANDARD_BANDS)
    ├── crossfeed.rs  # Bauer binaural crossfeed
    ├── saturation.rs # tape/tube/transistor waveshaping + highpass exciter + optional oversampled antialiasing
    ├── convolver.rs  # FFTConvolver: overlap-save short IRs + partitioned long IRs
    ├── dynamic_loudness.rs # ISO 226 Fletcher-Munson compensation
    ├── spectrum.rs   # FFT spectrum analyzer
    ├── resampler.rs  # SoX VHQ Resampler / StreamingResampler
    ├── automix_analysis.rs # offline automix analysis
    ├── lockfree_params.rs  # atomic parameter snapshots (RT boundary)
    ├── adapters.rs   # AudioProcessor adapters over each processor
    ├── downmix.rs    # Downmixer + DownmixCoefficients (pre-chain layout mapping)
    ├── dsp_chain.rs  # DspChain: composable processing chain
    ├── traits.rs     # AudioProcessor, ProcessResult
    ├── loudness_db.rs        # optional `loudness-db` feature (SQLite)
    ├── loudness.rs   # loudness module root + public re-exports
    └── loudness/
        ├── meter.rs      # EBU R128 LoudnessMeter + TruePeakDetector
        ├── normalizer.rs # LoudnessNormalizer
        ├── limiter.rs    # PeakLimiter (lookahead sample-peak; see audit)
        ├── ramp.rs       # GainRamp
        ├── info.rs       # LoudnessInfo
        └── atomic_state.rs # AtomicLoudnessState
```

```
benches/    # custom-harness benches (harness = false), run with --quick
examples/   # resample_sine, equalizer_curve (no audio files / features needed)
```

## Module Conventions

- A module is either a single `foo.rs` or a `foo.rs` root plus a `foo/`
  directory of submodules (see `decoder.rs` + `decoder/`, `loudness.rs` +
  `loudness/`). Both forms are in use; follow the neighbouring style.
- `processor/mod.rs` is the single curation point: submodules are private
  (`mod eq;`) and the intended surface is re-exported with `pub use`. The
  unified-abstraction modules (`adapters`, `dsp_chain`, `lockfree_params`,
  `traits`) are `pub mod`.
- `lib.rs` re-exports only the curated top-level surface; do not make a
  submodule `pub` just to reach one type — re-export it.
- Feature-gated code uses `#[cfg(feature = "...")]` at the module and item
  level (`loudness_db` behind `loudness-db`, network types behind `http`).

## Where New Code Goes

- A new DSP processor → `src/processor/<name>.rs`, wired through `mod.rs`
  re-exports, with an `adapters.rs` adapter and (if tunable) a
  `lockfree_params.rs` snapshot type.
- New decoder behavior → `src/decoder/`.
- A new benchmark → `benches/<name>.rs` plus a `[[bench]] harness = false`
  entry in `Cargo.toml`.
