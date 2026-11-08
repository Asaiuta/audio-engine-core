# Public API and streaming guide

This inventory describes the current core package. Streaming APIs are public;
applications do not need to use whole-track PCM buffers to compose them. The
crate supplies processing primitives, not an audio-device or player service.

## Capabilities and entry points

Paths below are relative to the audio_engine_core crate.

| Capability | Public entry points | Execution and storage contract |
| --- | --- | --- |
| Local / HTTP identity | MediaLocation, HttpMediaLocation | Explicit routing; HTTP transport requires the http feature |
| Streaming decode | StreamingDecoder::open, decode_next_borrowed, decode_next_into | Off-callback I/O and packet handling; borrowed PCM reuses decoder staging; into appends, so consume/clear between packets |
| Opening / metadata / cancellation | decoder::{OpenedMediaSource, StreamingDecoderBuilder, AudioInfo, TrackMetadata, DecodeCancelToken, HttpCredentials}; open_with_credentials_and_cancel, probe_opened_source | Source open, probe and decoder build can be separate; staging_buffer_bytes reports PCM staging only, not the complete source/codec working set |
| Seek / position | StreamingDecoder::seek, current_frame, info | Coarse seek in seconds; realized position in input frames; not sample-exact seeking |
| Streaming resampling | StreamingResampler; process_checked, finish_checked, StreamingProcessor::reset | Reusable state and caller buffers; advance consumed and produced frames separately, drain at EOF |
| Canonical playback DSP | CallbackSpec, PlaybackConfig, PlaybackPipeline::process | Prepared, in-place blocks at one sample rate; decoding, resampling and device negotiation are separate |
| Live controls / lifecycle | PlaybackController, PlaybackParameters | Volume/mute, IIR/FIR EQ, limiter, saturation, crossfeed, dynamic loudness, noise shaping; reset/drain/fade-stop requests with generation acknowledgement |
| Custom DSP graph | processor::{adapters, traits, lockfree_params}, DspChain, OutputChainBuilder | StreamingProcessor process/finish/reset with backpressure, latency and tail declarations; create/configure outside callback |
| DSP components | processor::{IirEqualizer, FirEq, FFTConvolver, ConvolverControl, Crossfeed, Saturation, DynamicLoudness, PeakLimiter, NoiseShaper} | Block processing; dynamic convolution publication/reclamation belongs to the control side |
| Loudness / true peak | analysis::{LoudnessMeter, TruePeakDetector}; LoudnessNormalizer, AtomicLoudnessState | Meter.process accepts incremental interleaved blocks; normalizer supports measurement/gain modes; analyze_track explicitly consumes a whole caller-provided slice |
| Visualization | analysis::{SpectrumAnalyzer, SpectrumConfig, WindowFunction} | push accepts mono blocks; spectrum / spectrum_db borrow the latest bins; bounded windows, warm-up and publication cadence |
| Spectral / signal descriptors | analysis::{DescriptorAnalyzer, DescriptorConfig, SpectralDescriptors, BandMeasurements, SignalMeasurements} | Mono push plus spectral/signal/bands/power_spectrum access; raw power and measurements, not display-normalized spectrum |
| Mel / MFCC | analysis::{MelAnalyzer, MelConfig, MfccAnalyzer, MfccConfig} | Incremental mono input; latest frame/mel/coefficients; choose the named Mel frontend explicitly |
| HPSS evidence | analysis::{HpssAnalyzer, HpssConfig, HpssFrame} | Streaming spectral masks and fractions with look-ahead; does not produce separated audio stems |
| Pitch evidence | analysis::{PitchAnalyzer, PitchConfig, PitchFrame, PitchCandidate} | Streaming single-source YIN F0 and pYIN stage-1 candidates; not polyphonic transcription or a complete melody/beat tracker |
| Channel geometry / downmix | audio_block::{AudioBlockRef, AudioBlockMut}, ChannelLayout, ChannelPosition, Downmixer, DownmixCoefficients | Borrowed interleaved geometry and layout-aware mixing; unknown speaker roles are not guessed |
| Ring storage | RingBuffer | Fixed-capacity sample storage; overflow drops oldest frames; mutable methods require ownership/synchronization supplied by the caller, not a split lock-free SPSC queue |
| Persistent loudness cache | LoudnessDatabase, TrackLoudness, LoudnessSourceIdentity, DatabaseStats | SQLite and filesystem operations off callback; loudness-db feature only |
| Budgets / floating-point runtime | decode_budget::decode_memory_budget; runtime::{audio_thread_init, audio_thread_float_mode_is_enabled, flush_subnormal_sample} | Per-operation decode limit and thread-local denormal handling; not a process-global allocator budget |
| Offline conversion / rendering | StreamingDecoder::decode_all, Resampler::resample_parallel, OutputRenderChain::render* | Return whole outputs; block-wise internals do not make total storage independent of track length |

The analyzer APIs generally expose the latest frame, not an event queue. A
large push can publish multiple frames while frame() returns only the last one;
consumers needing every frame must feed at the documented hop cadence and read
at each publication. Streaming input alone is not a promise that a costly
analyzer belongs inside the audio callback. Follow its individual realtime
contract and budget the analysis-worker workload separately.

## A bounded streaming composition

Run the included example:

~~~bash
cargo run --release --example stream_file -- path/to/music.flac 44100
~~~

[stream_file.rs](../examples/stream_file.rs) composes public APIs as:

~~~text
local file -> borrowed decoder packet -> streaming resampler
           -> PlaybackPipeline -> LoudnessMeter -> frame count / result
~~~

It retains the current packet and a 1024-frame output block, uses progress
counts for partial consumption, drains the resampler and DSP in order, and
never collects the complete PCM output. All operations run synchronously on
the example's worker/main thread; no device is opened. A real player supplies
a bounded cross-thread queue, device callback and underflow policy. Avoid
placing a mutex around the provided RingBuffer on an audio callback.

The example pipeline has only volume enabled. A custom chain with latency or
effect tails must account for those declarations when comparing source and
rendered durations. The older resample_sine example collects a short synthetic
output to check its frame count; that collection is a property of the example,
not a requirement of StreamingResampler.

## HTTP storage and cancellation

With http enabled, a valid Range origin supplies bounded range reads. An
origin that ignores Range or returns invalid partial metadata falls back to a
complete GET into an automatically deleted temporary file using a 64KiB copy
buffer. It still waits for that GET to finish before decoding. Temporary
storage needs disk space; failure is returned, never converted to a full RAM
fallback. File creation/write/seek failures use DecoderError::FileOpen;
transport failures retain NetworkError and cancellation is DecoderError::Canceled.

The existing total non-Range download admission limit is preserved:
decode_memory_budget().limit_bytes / 8, by default 256MiB. This now bounds
stored source bytes, not RAM. The decode budget itself defaults to 2048MiB per
whole-output decode and is not a shared reservation across instances. OS file
cache, codec state, HTTP/TLS buffers and caller queues are separate memory.
Cancellation is checked around reads; it does not interrupt an already blocked
reqwest read before that operation completes or times out.

## Features and boundaries

Streaming here means incremental decoded audio and seekable HTTP media objects.
The crate has no HLS/DASH playlist or segment scheduler, no ICY metadata
handling, and no Subsonic/Jellyfin/Emby session API. Consumers resolve server
identities and credentials before supplying a media location.

- Default: http + loudness-db + rubato (the owned Rust CoreSinc backend).
- Local-only: default-features = false, features = ["rubato"]. Decoder, DSP,
  analyzers and geometry remain; reqwest/tempfile and persistent SQLite cache
  are absent from the package's normal dependency graph.
- soxr selects native SoXR and wins if both backends are enabled. raw-rubato
  enables only the upstream benchmark control. At least one runtime backend
  must be selected.
- Devices, play/pause scheduling, queue/playlist management, output format
  conversion/IPC/UI and worker orchestration belong to consumers. DSP
  drain/fade-stop requests are not an operating-system playback service.
- AutoMix tempo/beat/structure/placement lives in the separate audio-automix
  package; importing core does not load research models. HPSS masks and pitch
  candidates in core do not imply stem separation or accepted music-analysis
  accuracy on arbitrary recordings.

For implementation-level contracts see rustdoc, [quality](quality.md), and the
[README streaming lifecycle](../README.md#streaming-processor-model).
