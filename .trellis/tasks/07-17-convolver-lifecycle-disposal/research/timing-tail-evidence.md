# Convolver Timing And Tail Evidence

## Current streaming contract

`FFTConvolver` produces causal output aligned to the current input block. Both
overlap-save and uniform partitioned routing hide their FFT block mechanics and
do not intentionally prepend an algorithmic delay. The adapter therefore uses
the trait default zero latency.

For an FIR with `L` frames, the semantic response after the last input frame is
exactly `L - 1` frames. `ConvolverProcessor::tail()` already reports this in
its current sample-rate domain, and `finish()` feeds zeros through the adopted
kernel until that count reaches zero.

Existing evidence was useful but narrow:

* `convolver_finish_preserves_last_frame_impulse_tail` covers one mono 3-tap IR
  with one-frame finish buffers;
* output-chain tests carry a convolution tail through limiter and resampler and
  compare different offline block sizes;
* partitioned core tests compare against overlap-save, which verifies routing
  parity but is not an independent mathematical oracle.

The adapter now adds an independent nested-loop oracle in
`convolver_process_and_finish_match_independent_direct_oracle`. It covers
one-tap mono, short stereo overlap-save, and long mono/stereo partitioned IRs,
whole-buffer and irregular chunks, one/odd/large finish buffers, exact
`IR - 1` tails, stable `Finished(0)`, and zero latency. The companion
`convolver_reset_isolates_prior_process_and_partial_finish_history` test covers
reset after partial process/finish, and
`convolver_sample_rate_only_retags_finite_tail_duration` covers rate-domain
metadata without changing the adopted generation.

## Independent oracle

For interleaved per-channel FIR taps, compute direct causal convolution:

```text
y[c, n] = sum(k = 0..L-1, x[c, n-k] * h[c, k])
```

Treat out-of-range input as zero and compare the concatenated ordinary-process
plus finish output against all `input_frames + L - 1` reference frames. This
simultaneously verifies zero latency, sample content and exact tail length.

The oracle must be structurally independent: simple nested loops over source
frames/taps, not calls into either FFT strategy or copied overlap-save logic.

## Coverage matrix

* Mono and stereo interleaving.
* One-tap (`tail=None`), short overlap-save IR, and long IR above
  `PARTITIONED_CONVOLUTION_IR_THRESHOLD`.
* Whole-buffer and irregular process chunks.
* Finish buffers of 1, odd and larger sizes; final `Finished(n)` followed by
  stable `Finished(0)`.
* Disabled/no-adopted-kernel reports no tail.
* Reset after partial process/finish matches a fresh processor with the same
  adopted kernel and does not leak the previous stream.
* Sample-rate update changes only the tail duration tag, not IR frames or
  current kernel/control configuration.

## Offline composition

The existing stage-complete renderer must remain authoritative: it processes
all convolver input, drains `L-1` frames, and sends both through downstream
limiter/resampler before those stages finish. Tests must retain last-frame
impulse survival, block-size-independent content, final-rate timing metadata
and `tail_truncated=false` for finite FIR tails.

## Performance evidence

Lifecycle control changes should not alter FFT routing or inner convolution
loops. Use the versioned callback/FIR quick reports for compatible 10% median
regression checks and `audio_convolver_perf --quick --enforce` as a local
algorithm-path guard. Keep control-side kernel construction/destruction out of
callback ns/sample timing; separately report deterministic publication,
adoption, backpressure and reclamation work counts.
