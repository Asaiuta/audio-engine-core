# Final Audio Engine Benchmark Coverage And Results

## Scope verdict

The registered suite now has 13 custom-main probes and covers every required,
deterministic, crate-owned performance dimension identified by this task. That
does not mean every imaginable end-to-end audio metric is owned here. Device
callbacks, driver/DAC latency, live-network behavior, cold-cache storage, and
unbounded process RSS require a consuming application or a dedicated external
environment and remain explicit exclusions.

## Coverage matrix

| Performance surface | Primary evidence |
| --- | --- |
| Callback aggregate throughput | `audio_callback_chain_perf` |
| Per-callback p95/p99/p99.9/max and missed deadlines | `audio_callback_tail_perf` |
| Offline render CPU and Rust allocation | `audio_output_render_perf` |
| Streaming resampler throughput and rate/quality/phase matrix | `audio_resampler_streaming_perf`, `audio_resampler_matrix_perf` |
| Convolution throughput, callback bursts, setup, drain, and ownership | `audio_convolver_perf`, `audio_lifecycle_memory_perf` |
| FIR design/regeneration and apply | `audio_fir_eq_perf` |
| Decoder open/probe/build/first PCM/steady decode/seek/allocation | `audio_decoder_perf`, `audio_gapless_comparison_perf` |
| Spectrum, downmix, loudness, true peak, AutoMix, ring buffer, SQLite cache | `audio_component_perf` |
| Processor setup/reset/finish, persistent Rust bytes, bounded lifecycle growth | `audio_lifecycle_memory_perf` |
| Lock-free control snapshot reads | `audio_lockfree_params_perf` |
| Objective signal correctness and quality | `audio_quality_measurements` |

The callback probes cover the canonical volume, EQ, saturation, crossfeed,
Convolver, dynamic-loudness, peak-limiter, and noise-shaper chain. The component
probe closes analysis/control-path gaps without duplicating that chain. SoXR and
Rubato remain separate baseline identities.

## Final quick results

All values below are from sequential Windows/x86_64 quick runs on 2026-07-26.
They are local observations, not portable absolute thresholds.

| Surface | SoXR/default median | Rubato-only median or status |
| --- | ---: | ---: |
| Decoder local open | 27.31 us | 27.18 us |
| Decoder probe/build/first PCM | 9.79 / 9.19 / 7.43 us | 10.10 / 9.18 / 5.66 us |
| Decoder steady PCM/WAV | 19.79 ns/frame, 50.52M frames/s, 1052.5x realtime | 19.48 ns/frame, 51.34M frames/s, 1069.5x realtime |
| Spectrum 1,024 | 5.05 ns/input sample | 5.48 ns/input sample |
| 5.1 downmix | 4.72 ns/frame | 4.66 ns/frame |
| Loudness 4,096 frames | 42.37 ns/input sample | 43.37 ns/input sample |
| True peak contiguous | 9.96 ns/input sample | 9.73 ns/input sample |
| AutoMix Head / Full | 54.42 / 108.18 ms | 54.26 / 108.00 ms |
| SQLite 128-row batch | 8.08 us/row | excluded because `loudness-db` is absent |
| Active resampler setup/reset/drain | 457.5 / 381.9 / 66.8 us | 114.1 / 1.2 / 25.8 us |
| Convolver setup 256 / 8,192 frames | 28.4 / 353.5 us | 26.6 / 352.3 us |
| Bounded ownership soak | 5 x 128 cycles, 0 retained Rust bytes per trial | 5 x 128 cycles, 0 retained Rust bytes per trial |

The final unpinned callback-tail report retains 48,000 callbacks across 12
cases, with no modeled deadline miss. At 512 frames, active DSP without
Convolver measured 121.6 us p99, 237.7 us p99.9, and 689.9 us max; with the
256-tap Convolver it measured 135.9 us p99, 258.5 us p99.9, and 361.5 us max.
Pinned same-machine evidence remains the only strict tail-regression source.

## Report integrity and baselines

The seven current compatible candidate reports contain 79 comparisons and zero
failures: 24 pinned callback-tail comparisons, 14 decoder comparisons, 27
component comparisons, and 14 lifecycle comparisons. Lifecycle retains all 13
cases but compares only seven stable timing cases. Every final quick report has
unique complete case keys, exact raw-sample lengths, valid work evidence, and a
successful concrete-type JSON read-back.

Both lifecycle reports contain nine Rust allocation rows and three persistent
memory rows. Reset, finish, and audio-side Convolver adoption record zero Rust
allocator operations. Publication retains a 168-byte Rust ownership wrapper;
control-side reclamation performs seven deallocations. SoXR native allocations
remain invisible to the Rust global allocator and none of these rows claims
process RSS.

## Quality gates

- Both default/all-features and Rubato-only bench compilation pass.
- Both strict all-target Clippy matrices pass with `-D warnings`.
- All-features passes 351 library tests, 18 benchmark-support tests, 3 Windows
  runtime tests, and 2 doctests.
- Rubato-only passes 378 library tests, 18 benchmark-support tests, 3 Windows
  runtime tests, and 2 doctests when eight unrelated nonlinear resampler WIP
  failures are skipped.
- Every task-owned Rust file passes `rustfmt --check`. Repository-wide rustfmt
  is blocked only by `contiguous_polyphase_backend.rs` and `rubato_backend.rs`,
  which belong to the separate nonlinear resampler WIP.

The eight unrelated Rubato failures are:

1. `contiguous_output_matches_polyphase_oracle_within_1e_minus_9`
2. `contiguous_mono_matches_stereo_channels_exactly`
3. `contiguous_process_and_reset_are_allocation_free_after_setup`
4. `contiguous_reset_restores_a_fresh_stream`
5. `nonlinear_polyphase_direct_and_staged_streams_are_bit_exact_and_duration_aligned`
6. `nonlinear_polyphase_clear_after_partial_drain_matches_fresh`
7. `nonlinear_polyphase_process_and_drain_do_not_allocate_after_setup`
8. `nonlinear_phase_is_real_and_reports_causal_latency`

Six hit the history-window assertion at
`src/processor/resampler/contiguous_polyphase_backend.rs:267`; the two
no-allocation tests attempt a 30-byte allocation. No file in that WIP was
modified by this benchmark task.

## Explicit exclusions

- CPAL/WASAPI negotiation, device callback scheduling, driver/DAC latency, and
  play-button-to-sound latency.
- Live HTTP throughput, network jitter, and cancellation response distributions
  in deterministic quick mode.
- Cold-cache filesystem latency and a broad compressed-codec corpus; the quick
  decoder fixture is warm-cache PCM/WAV, while the gapless comparator remains
  the optional codec-focused evidence path.
- Native SoXR heap attribution, process RSS, and unbounded soak/leak claims.
- Universal nanosecond thresholds across unrelated machines or environments.
