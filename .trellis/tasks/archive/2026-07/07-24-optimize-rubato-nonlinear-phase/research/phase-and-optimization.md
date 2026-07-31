# Nonlinear Phase and Rubato Optimization Research

## Starting Constraints

At task start, `rubato_backend.rs` used Rubato 4 `Fft<f64>` for common
Low/Standard/High ratios and `Async<f64>` sinc for UltraHigh or pathological
ratios. The adapter used exact 1024-frame internal chunks and fixed
two-subchunk FFT construction. Both built-in engines are linear phase, so real
`PhaseResponse` support could not be implemented by changing `latency()` or by
shifting the output.

The streaming layer already has the required setup/process split: backend
construction allocates, while `process`, `drain`, and `reset` use preallocated
buffers. Any nonlinear implementation must preserve this split.

## Candidate Designs

### A. Post-filter or all-pass correction

Rejected. A delay is still linear phase. A generic all-pass can change phase,
but it does not turn the existing linear-phase anti-alias filter into the
minimum-phase filter with the same magnitude, and it adds another uncontrolled
frequency response/state boundary.

### B. Reuse Rubato with a phase parameter

Rejected for Rubato 4. The public constructors expose sinc length, window,
interpolation, and ratio, but no custom FIR coefficient bank or phase-response
input. Passing `PhaseResponse` through the existing constructor cannot change
the kernel.

### C. Setup-designed real-cepstrum polyphase FIR

Recommended. Build a low-pass prototype on a sufficiently oversampled grid,
derive its minimum-phase spectral factor with the real cepstrum, and split the
result into rational polyphase branches. Maximum phase is the reverse of the
minimum-phase kernel. The coefficient bank is immutable after setup; process
only performs bounded multiply-accumulate work over preallocated history.

This is the same core spectral-factorization method already used by the
project's `FirEq::generate_minimum_phase_ir`, but resampler coefficients must
be normalized for the rational high-rate domain and must not receive the EQ
module's presentation-only tail window.

## Phase Verification

For a fixed impulse and ratio, tests must check all of:

* Magnitude response at DC, passband, transition, and stopband remains within
  the documented tolerance for all three modes.
* The minimum-phase impulse has an earlier energy centroid than the linear
  prototype; maximum has a later centroid.
* A pure shift is rejected by comparing centered/shift-compensated phase or by
  comparing normalized impulse envelopes after best delay alignment.
* Streaming chunking and drain produce the same phase kernel as a one-shot
  render, within the documented floating-point tolerance.

The final lifecycle regression
`nonlinear_chunking_interleaving_terminal_and_reset_match_references` compares
whole-input and irregularly chunked renders bit-for-bit, compares both channels
against independent mono engines, checks repeated terminal finish, and verifies
that reset output is bit-exact with a fresh nonlinear instance.

## Optimization Evidence

The archived pre-native sweep found that four FFT subchunks improved 44.1->48
relative to two, but regressed 48->96; one subchunk was poor for both. This
supports a ratio-specific decision, but the sweep must be repeated with the
retained native-interleaved implementation before changing the constant.

The current adapter moves input and output with `Vec::copy_within` after each
fixed backend chunk. A bounded ring/offset representation is a plausible
optimization, but it must retain exact chunking, reset, drain, and no-allocation
behavior. Direct-to-caller output is a second optimization candidate.

### 2026-07-24 retained and rejected candidates

The ratio-specific four-subchunk candidate was rejected. Under the adjacent
high-load runs its 512-frame checked 44.1->48 median was 16.439 ns/input sample,
while restoring two subchunks measured 13.146. The unchanged 48->96 case also
showed substantial system noise, so the retained implementation keeps two
subchunks globally.

The integer-ratio direct-output path was retained. It bypasses
`out_stage -> out_fifo -> caller` only when one 1024-frame input chunk maps to
an exact integer number of output frames. This excludes 44.1->48 and other
ratios where Rubato's fixed FFT blocks temporarily overproduce frames that the
duration-aligned drain must later truncate. It includes the target 48->96 case.

Adjacent quick reports with the branch enabled and temporarily disabled gave
the following `process_checked` medians:

| 48->96 buffer | Direct output | Staged output | Improvement |
| --- | ---: | ---: | ---: |
| 128 frames | 13.630 ns/input sample | 16.583 | 17.8% |
| 256 frames | 12.216 | 20.380 | 40.1% |
| 512 frames | 12.438 | 27.870 | 55.4% |

The host remained noisy, so these values are retained as directional A/B
evidence rather than a replacement for the earlier clean absolute baseline.
Both reports passed their work-validity gates. The direct path also passed the
one-shot/streaming length parity, irregular chunking, reset, and no-allocation
tests after its ratio guard was added.

### Direct-output drain-accounting regression and fix

The first enforced quality run after retaining direct output passed 26 of 27
gates. The 96->48 kHz worst-alias result regressed to `-99.33 dB`, even though
the direct process output itself matched the established engine behavior.

The root cause was lifecycle accounting rather than filter quality. The staged
path advances `MonoBackend::emitted` when `emit_up_to` copies frames to the
caller, but the new direct path returned caller-visible frames without
advancing the same counter. `drain` therefore treated those frames as still
pending and generated them again during `finish`, corrupting the completed
sequence and the alias measurement. The direct branch now increments
`emitted` exactly once before returning its produced count.

`duration_stable_direct_output_is_bit_exact_with_staged_output` is the focused
regression. It compares output length and every `f64::to_bits()` value between
the direct path and a deliberately output-constrained staged path for both
48->96 kHz High FFT and 96->48 kHz UltraHigh sinc. This covers integer-ratio
upsampling and downsampling as well as both retained linear engines.

After the fix, the enforced quick quality probe passed all 27 gates and the
96->48 kHz worst-alias attenuation returned to `-208.11 dB`. The enforced
streaming performance probe also passed with algorithm identifier
`rubato_streaming_native_interleaved_fft_sinc_direct_integer_ratio`.

Post-fix evidence:

* `research/quality-post-fix-quick.json`
* `research/resampler-post-fix-quick.json`

### Fixed-capacity ring FIFO retained

The input/output FIFO shift-removal candidate was retained after an adjacent
heavy A/B. The first quick runs were too short to decide: scheduler load moved
48->96 `process_checked` medians between roughly 11 and 22 ns/input sample even
without another code change. Those four quick reports remain as raw evidence,
but the decision uses the 1,350-iteration, 15-trial heavy pair collected on the
same compiler, target, CPU, profile, and feature set.

| Scenario | Buffer | Ring FIFO | Moving `Vec` FIFO | Improvement |
| --- | ---: | ---: | ---: | ---: |
| 44.1->48 kHz | 128 | 8.346 ns/input sample | 9.492 | 12.1% |
| 44.1->48 kHz | 256 | 10.471 | 11.729 | 10.7% |
| 44.1->48 kHz | 512 | 8.200 | 11.727 | 30.1% |
| 48->96 kHz | 128 | 10.404 | 15.031 | 30.8% |
| 48->96 kHz | 256 | 11.174 | 12.682 | 11.9% |
| 48->96 kHz | 512 | 10.984 | 18.113 | 39.4% |

The retained `SampleRing` owns a setup-allocated `Box<[f64]>`, never grows or
overwrites unread samples, and performs each push/pop with at most two
contiguous copies. The input ring is exactly two backend chunks: because every
backend call consumes one complete chunk, the next front chunk remains
contiguous even across wrap. The output ring retains strict backpressure rather
than dropping old audio. This deliberately does not reuse `pipeline::RingBuffer`,
whose overwrite-on-full, logging, and non-consuming read behavior are not the
resampler adapter contract.

This removes steady-state input/output FIFO `copy_within` shifts. The separate
one-time leading-delay compaction inside a newly generated output stage remains
because it is not queued FIFO movement. The algorithm identifier is now
`rubato_streaming_native_interleaved_fft_sinc_direct_integer_ratio_ring_fifo`.
Focused wrap/contiguity tests, random chunking, reset, direct/staged parity, and
process/finish no-allocation coverage protect the retained layout.

Ring A/B evidence:

* `research/resampler-ring-candidate-heavy.json`
* `research/resampler-ring-baseline-heavy.json`
* `research/resampler-ring-candidate-quick.json`
* `research/resampler-ring-candidate-repeat-quick.json`
* `research/resampler-ring-baseline-quick.json`
* `research/resampler-ring-baseline-repeat-quick.json`

Final retained-code verification passed both feature matrices. The pure-Rust
run passed 360 library tests, 10 benchmark-support tests, 3 Windows runtime
tests, and 2 doctests; the all-features run passed 351 + 10 + 3 + 2. Strict
Clippy passed for both matrices, and rustfmt/diff checks remained clean. The
final enforced quality report passed 27/27 gates with 96->48 kHz worst alias
attenuation unchanged at `-208.11 dB`. The final enforced quick streaming
report passed its work gates under the ring algorithm identifier; its
`process_checked` medians were 9.365/14.099/9.375 ns/input sample for
44.1->48 kHz and 12.240/12.268/12.867 for 48->96 kHz at 128/256/512 frames.
Those quick medians remain load-sensitive, so the adjacent heavy table above is
the acceptance evidence.

Final evidence:

* `research/quality-ring-final-quick.json`
* `research/resampler-ring-final-quick.json`

## Risks and Limits

* Very large reduced ratios create large polyphase banks or poor transition
  resolution. The nonlinear path should reject unsupported extremes rather than
  silently use linear phase.
* Maximum phase is causal but has a real leading delay. The lifecycle latency
  contract must report or explicitly compensate that delay; cropping it without
  retaining the phase response would be incorrect.
* Longer kernels can improve numerical stopband attenuation but increase setup
  memory and CPU. Acceptance should prioritize audible/quality thresholds over
  matching a SoXR numeric floor.
