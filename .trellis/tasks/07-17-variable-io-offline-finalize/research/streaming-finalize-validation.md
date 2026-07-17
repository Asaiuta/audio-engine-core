# Streaming Finalize Validation

## Correctness evidence

Focused tests cover the failure mechanisms owned by this task:

* `processor::resampler::tests`: 512-frame and 100,000-frame 48->96 kHz
  inputs produce exactly 2x frames after native drain; irregular input chunks
  match one-shot feed; terminal finish is idempotent; native clear isolates a
  subsequent silent stream; process and finish allocate no Rust heap memory. A
  4,096-frame impulse stream places an input-frame-1,024 peak at output frame
  2,048 within one frame and confirms that the drained sequence needs no
  offline timeline crop.
* `processor::adapters::tests`: limiter finish emits exactly its declared
  look-ahead latency; convolver finish emits exactly `IR length - 1` semantic
  tail frames; both finite finish paths are allocation-free after setup.
* `processor::output_chain::tests`: a last-frame impulse survives compensated
  rendering; convolution tail passes through limiter and 48->96 kHz resampling;
  raw output after its declared latency is bit-identical to compensated output;
  unknown-tail energy termination is block-size independent; a persistent tail
  reaches the configured maximum and reports truncation. The energy detector
  runs inside finish generation: the decaying 100-frame-cap fixture stops after
  at most 28 frames with 7-frame blocks and 31 frames with 31-frame blocks,
  while producing identical retained output. It no longer computes the full
  safety cap before trimming.
* `cargo run --example resample_sine --offline` reports exactly
  `48000 frames @ 48000 Hz -> 44100 frames @ 44100 Hz`.

## Performance evidence

Command:

```text
cargo bench --bench audio_resampler_streaming_perf --offline -- --quick
```

Five quick runs for stereo 44.1->48 kHz, 512 input frames, checked unified
trait path:

| Run | ns/input sample | us/input buffer |
| ---: | ---: | ---: |
| 1 | 15.567 | 15.941 |
| 2 | 11.400 | 11.674 |
| 3 | 13.788 | 14.119 |
| 4 | 13.219 | 13.536 |
| 5 | 11.856 | 12.141 |
| Median | 13.219 | 13.536 |

Every measured run consumed all input. The benchmark now also checks total
produced frames against the expected stream ratio within a conservative 5%
window (native filter scheduling leaves a small bounded backlog until finish).

The earlier README value (`7.9 ns/input sample`) came from the removed
`process_chunk_*` path, which ignored Soxr's returned `input_frames` and could
silently drop unconsumed input. It is not a valid like-for-like performance
baseline because completing less work was one of the defect mechanisms.

## Final gates

* `cargo test --all-features`: 250 unit tests + 2 doctests passed.
* `cargo test --no-default-features`: 242 unit tests + 2 doctests passed.
* `cargo clippy --all-targets --all-features -- -D warnings`: passed.
* `cargo clippy --all-targets --no-default-features -- -D warnings`: passed.
* `cargo rustdoc --all-features --lib -- -D warnings`: passed.
* `cargo run --example resample_sine`: exactly 48,000 -> 44,100 frames.
* `cargo bench --bench audio_quality_measurements -- --quick --enforce`: all
  enforced metrics passed after exposing Cargo's cached MinGW runtime DLLs to
  the bench process. Representative results: 44.1->48 kHz THD+N `-187.01 dB`,
  20 Hz-18 kHz maximum response deviation `0.0013 dB`, limiter output
  `-1.00 dBTP`.
* `cargo package --allow-dirty --offline`: packaged 202 files (374.1 KiB
  compressed) and successfully rebuilt the packaged crate. The online form was
  unable to refresh the crates.io index because Windows Schannel had no client
  credentials; this was an index/environment failure, not a package-content
  failure.
