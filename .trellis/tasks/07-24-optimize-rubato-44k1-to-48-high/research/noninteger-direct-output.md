# Non-Integer-Ratio Direct Output Research

## Current path

`MonoBackend::process` first emits pending `out_fifo` frames, fills the
two-chunk input ring, and processes complete 1024-frame prefixes. Direct caller
output is permitted only when:

```text
(CHUNK_IN * to_rate) % from_rate == 0
```

For 44.1->48, the reduced ratio is 147:160 and one input chunk maps to
approximately 1114.558 output frames. The route therefore always processes
into `out_stage`, pushes the complete result into `out_fifo`, and pops it into
the caller.

The restriction is not merely conservative. Rubato may temporarily produce a
frame beyond or below a rational prefix's rounded duration. At finish, the
adapter truncates/pads to the complete stream duration. Returning every
per-chunk output directly would make `emitted` exceed the final duration; the
current `max(expected_total, emitted)` guard would then preserve invalid extra
frames rather than remove them.

## Recommended candidate

Track two distinct input counters:

* caller input accepted into the FIFO;
* real input consumed by completed backend chunks.

After processing a real chunk, calculate the maximum caller-visible prefix:

```text
allowed_total = round(processed_real_input * to_rate / from_rate)
direct_budget = allowed_total - emitted
```

When caller output has room for `engine.output_frames_max()` and `out_fifo` is
empty, run the engine directly into caller memory. Return at most
`direct_budget` frames and copy only the remaining produced tail into the
preallocated output ring. Frames staged in the ring are not `emitted` until a
later `emit_up_to` call copies them to the caller.

During drain, a padded backend chunk is not real input and must not increase the
prefix budget. The existing final `expected_total` remains authoritative for
finish extension and complete duration.

## Required invariants

* Delay-skipped samples never enter either direct output or spill.
* Every caller-visible frame increments `emitted` exactly once.
* Spill frames retain original order and are emitted before later direct work.
* Direct output never exposes samples beyond the processed-real-input budget.
* Caller input accepted but not yet processed cannot authorize output.
* Reset clears accepted/processed counters, spill, delay, and terminal state.
* Direct and staged routes produce the same final bits and duration.

## Evidence context

The retained integer-ratio direct branch previously improved 48->96 quick
medians by 17.8-55.4%, depending on caller block size. This is directional
evidence that removing two full-buffer copies can matter, not a prediction for
147:160.

The later fixed-ring heavy comparison measured 44.1->48 at 8.346/10.471/8.200
ns/input sample for 128/256/512 frames. Because this is already near the older
SoXR figure, the candidate should be retained only with adjacent same-revision
evidence rather than on architectural appeal.

## Alternatives

### FFT configuration sweep

Sweep fixed input chunks and sub-chunk counts. This is cheap to research but
changes FFT cost/latency and earlier four-sub-chunk results were not stable
under the retained native-interleaved architecture. It is not the first
candidate.

### Specialized 147:160 block polyphase

Precompute one 160-output/147-input phase schedule, store coefficients in
phase-major order, and evaluate block dots over mirrored history with SIMD.
This has the highest potential performance ceiling but changes the DSP engine
and greatly expands quality and lifecycle validation. Consider only if adapter
copy removal cannot meet the performance target.

## Result (2026-07-25, retained)

Same-revision heavy (1350x15) adjacent A/B on the working machine,
`music_44k1_to_48k` `process_checked` ns/input-sample medians:

| frames | baseline (`resampler-rubato-baseline-heavy.json`) | candidate (`resampler-rubato-candidate-heavy.json`) | delta |
| --- | --- | --- | --- |
| 128 | 18.554 | 11.292 | -39.1% |
| 256 | 18.860 | 10.931 | -42.0% |
| 512 | 24.073 | 10.904 | -54.7% |

Fresh SoXR reference (`resampler-soxr-baseline-heavy.json`): 16.796 / 16.354 /
15.068. The retention gate (>=5% at 512, no >5% regression at 128/256) passed,
so the prefix-budget direct candidate was retained and the benchmark algorithm
identity changed to
`streaming_native_interleaved_halfband2x_fft_sinc_direct_integer_and_prefix_budget_ring_fifo`.
This machine session was globally slower/noisier than the earlier recorded
8.2-10.5 ns/sample runs, so only the adjacent same-revision deltas are
authoritative.

