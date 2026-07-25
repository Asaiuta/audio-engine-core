# Research: FFT-based nonlinear-phase (minimum/maximum) streaming resampler design

- **Query**: Restructure the pure-Rust nonlinear-phase resampler from time-domain rational polyphase FIR (~761 ns/input-sample at 48→96 High/Minimum stereo) toward the ~4.7 ns of the Linear Rubato FFT path.
- **Scope**: mixed (internal code study + external algorithm references)
- **Date**: 2026-07-25

## 1. Current state (measured facts)

### Benchmarks (`target/bench-reports/resampler-matrix-rubato.json`, mode=quick, median ns/input-sample)

| Case (stereo, 512 frames) | rubato backend | soxr backend |
|---|---|---|
| 48k→96k High **Minimum** | **761.0** | 13.83 |
| 48k→96k High Linear | 4.68 (halfband) | 10.41 |
| 44.1k→48k High Linear | 7.4 (rubato Fft) | 10.91 |
| 44.1k→48k Standard Linear | 8.12 | 10.65 |
| 96k→48k High Linear | 5.34 | 4.74 |
| 44.1k→48k UltraHigh Linear (sinc) | 91.07 | 13.39 |

Target: get 48→96 High/Minimum from 761 ns to the 5–10 ns band.

### Current nonlinear engine — `src/processor/resampler/polyphase_backend.rs`

- Rational polyphase: `up = to/gcd`, `down = from/gcd`, bound `MAX_REDUCED_RATE = 1_024` (line 16). Coefficient bank cap `MAX_POLYPHASE_COEFFICIENTS = 524_288` = `up * taps_per_phase`.
- `taps_per_phase(quality)`: Low 64 / Standard 128 / High 256 / UltraHigh 512 (lines 198–205). Full up-rate kernel length `L = up * taps_per_phase` (48→96 High: 2×256 = 512; 44.1→48 High: 160×256 = 40 960).
- Kernel design at setup: Kaiser-windowed sinc prototype (`design_linear_prototype`, cutoff `0.5*rolloff/max(up,down)`), spectral factorization via `minimum_phase_from_log_magnitude` (real-cepstrum, in `src/processor/fir_design.rs`), maximum phase = time reversal, `normalize_kernel` to unit DC sum, per-phase coefficients scaled by `up` (`polyphase_coefficients`, line 294).
- Per-output inner loop (lines 153–177): for each output frame, per channel, `taps_per_phase` MACs through a **modulo-indexed interleaved history ring** (`(write_frame + history_frames - history_age) % history_frames`, line 168). The `%` per tap and interleaved striding are why it runs at ~1.5 ns/MAC: 48→96 High stereo does 256 taps × 2 ch × 2 outputs = 1024 MACs per input frame = 512 MACs per input *sample* → 761 ns.
- Lifecycle contract exposed to the adapter: `latency_frames = phase_peak_latency_frames(kernel, down)` (kernel |peak| index /down, rounded), `finish_extension_frames = (L-1).div_ceil(down)`, `output_delay() = 0`, output pacing is exact-rational (`next_output < total_input*up/down`), `output_frames_max = (chunk*up).div_ceil(down)+2`, `reset()` zeroes history/counters.

### Adapter — `src/processor/resampler/rubato_backend.rs`

- `RubatoEngine` enum: `Halfband | Sinc | Fft | Polyphase`. Nonlinear phase always routes to `Polyphase` (line 229–232). Linear FFT route gated by `should_use_fft`: not UltraHigh and reduced ratio components ≤ `MAX_FFT_REDUCED_RATE = 1024`.
- Adapter feeds engines exact `CHUNK_IN = 1024`-frame chunks from `in_fifo` (a `SampleRing` requiring `front_contiguous(CHUNK_IN*channels)`), stages engine output in `out_stage` (`output_frames_max()*channels`), spills into `out_fifo` (2× capacity).
- Direct-output fast path (skips `out_stage`/`out_fifo` copies): taken when `out_fifo` empty and caller output ≥ `output_frames_max()`, and either the ratio is duration-stable per chunk (`CHUNK_IN*to % from == 0`) or `prefix_budget_direct` (currently only for `RubatoEngine::Fft`, line 428). **The polyphase engine paces output exactly rationally per input frame, so a replacement engine that keeps exact rational pacing per chunk qualifies for `prefix_budget_direct` too.**
- Drain: `expected_total = expected_output_frames(total_input) + finish_extension_frames()`, pads zero chunks, `MAX_DRAIN_STALL_ROUNDS = 64`.
- Engines report `latency_frames`/`finish_extension_frames` only for `Polyphase`; the linear engines report 0 and instead use `output_delay()` (frames discarded at start). A causal min-phase FFT engine should follow the Polyphase convention: `output_delay()=0`, real `latency_frames`.

### Existing FFT machinery reusable in-repo

- `src/processor/convolver.rs`: `FFTConvolver` with `OverlapSaveConvolver` (IR ≤ 4096) and `PartitionedConvolver` (partition 1024, spread tail quanta) — realtime-safe, preallocated, `process_into` fixed-block. It convolves at a **single rate** (no rate change), so it cannot be used as-is for resampling, but `OverlapSaveConvolver` (lines 301–643) is the local reference implementation for overlap-save bookkeeping (channel chunk prep, overlap update, scratch layout).
- Dependencies already present (`Cargo.toml`): `rustfft = "6.2"`, `realfft = "3.5"` as direct deps (not just via rubato). `polyphase_backend.rs` already uses `rustfft::FftPlanner` at setup.
- `src/processor/resampler/halfband_backend.rs` (386 lines): the template for a hand-rolled engine variant plugged into `RubatoEngine` (same `process_chunk(input, output) -> (consumed, produced)` shape).

### Spec contracts the new engine must satisfy

- `.trellis/spec/backend/realtime-safety.md`: hot path = `process`/`drain`/`clear`; no allocation (all buffers in `new`), no locks/logging/IO, bounded per-call work, no panics; FFT plans must be created at setup (rustfft `FftPlanner::plan_*` allocates — setup-only; `process()` on a preplanned FFT with preallocated scratch is allocation-free).
- `.trellis/spec/backend/streaming-lifecycle.md`: `process_checked`/`finish_checked` progress contract, duration alignment, reset-to-fresh bit-exactness. Existing adapter tests enforce: chunked vs single-feed bitwise identity, no-alloc under `assert_no_alloc`, clear-after-partial-drain equals fresh.

## 2. Candidate structures

Notation: `L = up*taps_per_phase` (up-rate kernel length), `Lin = taps_per_phase` (kernel span in input samples), quality High → `Lin = 256`.

### (a) Zero-stuff → full-rate overlap-save with the whole min-phase kernel → decimate

Run overlap-save at the up-rate `fs*up` on the zero-stuffed signal, then keep every `down`-th sample.

- Cost: FFT blocks at rate `fs*up`. For 1:2 this is acceptable (2× oversampled FFTs). For 147:160 the intermediate rate is 44.1 kHz × 160 = 7.056 MHz; the FFT does not exploit the fact that 159 of 160 stuffed samples are zero, and 146 of 147 computed outputs are discarded. Cost blows up by ~`up` (and wastes `down` on discard): O(up · log) per input sample → hundreds of ns for 147:160.
- Verdict: fine only for tiny `up` (1:2), pointless generality. **Reject** as the general structure.

### (b) Per-branch overlap-save (polyphase decomposition in the frequency domain)

Decompose kernel into `up` branches of `taps_per_phase` taps each; all branches filter the *same* input stream, so one forward FFT of the input block is shared, then `up` spectrum-multiplies + `up` inverse FFTs; interleave and decimate by `down`.

- Cost per input sample ≈ (1 forward + `up` inverse FFTs)/block, with only `1/down` of interleaved outputs kept. For 1:2 (up=2, down=1): 1 forward + 2 inverse ≈ excellent (~1.5× the linear FFT path). For 147:160: 160 inverse FFTs per block, ~159/160 of the work per retained output wasted after decimation → ~100+ ns/input-sample.
- Verdict: good for `up ≤ ~4` only. Subsumed by (d), which is strictly better. **Reject.**

### (c) Keep time-domain polyphase, use FFT only for long per-branch dot products

Per-branch dot product is only 256 taps; FFT pays off for a *batch* of outputs sharing one branch, but at 147:160 each branch produces one output every 147 input samples — no batching within a chunk. Only restructures constants, does not remove the O(taps) per output.

- Realistic gain here is not FFT but **fixing the inner loop**: remove the per-tap `%` (copy the needed contiguous history window per output, or keep a linear history with memmove per chunk), deinterleave channels, let LLVM autovectorize. Expect 5–10× (761 → ~80–150 ns), still far from 5 ns.
- Verdict: useful cheap fallback optimization for ratios that cannot route to FFT, not the main design. **Keep as optional follow-up.**

### (d) Spectral rational resampling with a complex (minimum-phase) filter spectrum — the rubato-`Fft`/soxr structure ⭐

This is what rubato's `Fft` engine and soxr's DFT stages do for linear phase; nothing in the structure requires linear phase — the filter is applied as a **complex spectrum multiply**, so substituting the minimum-phase kernel's spectrum gives nonlinear phase at identical runtime cost.

Structure (per channel, per internal block):

1. Overlap-save at the **input** rate: real FFT of size `Nin = 2*nin` over `nin` new input samples + `nin` history (`nin` is a multiple of `down`).
2. Multiply the `Nin/2+1` input bins by the precomputed complex kernel spectrum `H[k]` and map/fold bins onto the output grid of size `Nout = 2*nout`, `nout = nin*up/down` (zero-stuffing by `up` is spectrum replication; low-pass kills the images, so below cutoff each output bin takes exactly one input-image bin; above cutoff bins are zero).
3. Inverse real FFT of size `Nout`; keep the last `nout` samples (overlap-save discard), scale by `up/Nin·Nout` normalization.

`H[k]` is the `Nup = Nin*up`-point FFT of the **zero-padded time-domain minimum-phase kernel** (the exact same kernel `minimum_phase_prototype` already produces), sampled on the retained bins. Because the kernel is real and causal but not symmetric, `H` is complex — that is the whole difference from the linear path.

- Correctness condition (no circular aliasing): overlap ≥ kernel span ⇒ `nin ≥ Lin` (`≥ 256` High, `≥ 512` UltraHigh) at the input rate — equivalently `Nin ≥ nin + Lin`.
- Cost is **independent of up/down magnitudes** (only via FFT sizes `2·down·s` / `2·up·s`): one forward + one inverse real FFT + one spectrum pass per `nin` input samples.

### (e) What known libraries do (external reference)

- **soxr**: rate conversion = cascaded DFT (overlap-save FFT) stages + polynomial variable-rate stage; the `SOXR_MINIMUM_PHASE` / `SOXR_INTERMEDIATE_PHASE` flags change only the **filter design** (half-band filters made minimum-phase via the same cepstral technique) applied inside the identical DFT stages (see soxr `filter.c` `lsx_fir_to_phase`, ported conceptually as this repo's `minimum_phase_from_log_magnitude`). Its 13.8 ns 48→96 min-phase result in `resampler-matrix.json` is empirical proof the FFT-stage-with-min-phase-kernel structure hits the target band.
- **libsamplerate**: pure time-domain polyphase sinc, linear phase only — not a useful model here.
- **rubato 4 `Fft`**: `FixedSync::Input`, `CHUNK_IN=1024`, `FFT_SUB_CHUNKS=2` per this adapter; linear-phase only because its filter spectrum is derived from a symmetric window design, not because of the engine structure.

## 3. Cost estimates (High quality, stereo, f64 realfft)

Real-FFT ≈ `2.5·N·log2 N` flops. Per input sample per channel: `(2.5·Nin·log2 Nin + 2.5·Nout·log2 Nout + ~6·Nin/2)/nin`.

| Ratio | nin / Nin / Nout | flops per input sample/ch | projected ns/input-sample (stereo, ~0.3–0.5 ns/flop effective incl. copies) |
|---|---|---|---|
| 1:2 (48→96) | 512 / 1024 / 2048 | ≈ 165 | **~4–8 ns** (linear halfband path measures 4.68; linear rubato-Fft ~5–7) |
| 147:160 (44.1→48) | 588 / 1176 / 1280 | ≈ 130 (mixed-radix penalty ~1.3×) | **~6–10 ns** (linear rubato-Fft measures 7.4) |
| 2:1 (96→48) | 512 / 1024 / 512 | ≈ 110 | ~4–7 ns |
| UltraHigh (512 taps) any of the above | nin doubled (1024-multiples) | ~same per-sample (log grows slightly) | ~5–12 ns (vs 761·2 ≈ 1.5 µs today) |

Time-domain polyphase per input sample/ch = `taps · up/down` MACs ≈ 512 (1:2 High) / ~279 (147:160 High). Even a perfectly vectorized 0.25 ns/MAC rewrite lands at ~70–130 ns — the FFT structure wins by ≥10× for every taps_per_phase ≥ 64 and every ratio it can serve. **Crossover: FFT wins whenever `taps_per_phase · up/down ≳ 32` MACs/sample — i.e. always at these quality tiers.**

## 4. Latency, finish extension, memory, realtime safety (structure (d))

- **Latency**: kernel is unchanged (same time-domain min-phase kernel), so keep `latency_frames = phase_peak_latency_frames(kernel, down)` exactly as `polyphase_backend.rs:311`. Overlap-save of a causal kernel adds **zero algorithmic delay**; `output_delay() = 0`. Maximum phase = reversed kernel, peak near the end → large latency, same as today.
- **Finish extension**: unchanged formula `kernel_finish_extension_frames(L, down) = (L-1).div_ceil(down)` — the drain zero-padding in the adapter flushes the overlap history exactly as it flushes the polyphase ring today.
- **Output pacing**: with `nin` a multiple of `down`, every block emits exactly `nin·up/down` frames — exact rational pacing per chunk, satisfying `expected_output_frames` alignment and making the engine eligible for `prefix_budget_direct` (like `RubatoEngine::Fft`) or even duration-stable direct when `CHUNK_IN·up % down == 0`.
- **Memory** per channel: input overlap buffer `Nin`, `Nin/2+1` complex input bins, `Nout/2+1` complex output bins, `Nout` inverse scratch, plus realfft scratch. High/147:160: ≈ (1176 + 589·2 + 641·2 + 1280)·8 B ≈ 39 KB/ch; shared: kernel spectrum `Nup`-grid retained bins ≈ `Nout/2+1` complex ≈ 10 KB. Total stereo ≪ 1 MB; trivially preallocated in `new`.
- **Per-call work bound**: adapter feeds fixed 1024-frame chunks; engine runs `1024/nin` (≤ ~2–4) blocks per chunk — hard bound, no data-dependent loops.
- **Realtime safety**: `FftPlanner`/`RealFftPlanner` planning + all `Vec`s in `new` (setup, allocation allowed); `process()` uses `process_with_scratch` on preallocated scratch — allocation-free (must pass the existing `assert_no_alloc` test pattern). No locks/logging. Reset = zero overlap buffers + counters.

## 5. Routing policy

- Route **all** nonlinear-phase requests with reduced `up, down ≤ MAX_REDUCED_RATE` (1024, same bound as today — check stays in `PolyphaseResampler`-equivalent constructor) to the new spectral engine, **all quality tiers including UltraHigh** (unlike the linear path, there is no "stronger sinc tier" to preserve: the nonlinear kernel family is ours, and the FFT applies the identical kernel bit-for-kernel).
- Keep the time-domain `PolyphaseResampler` only as the constructor-error fallback path is today: ratios beyond the bound already error out; no silent fallback needed. Optionally retain it behind a test-only comparator (see §6). If a conservative rollout is preferred, gate: FFT for `up,down ≤ 1024` (i.e. everything currently accepted) and delete nothing.
- Cost crossover justification: time-domain per-sample cost `taps·up/down ≥ 64` MACs for every supported quality even at heavy downsampling; FFT cost ~110–170 flop-equivalents — FFT never loses within the accepted ratio space. The only regime where time-domain could win is extreme downsampling (`up/down ≪ 1/8`) at Low quality (64·(1/8) = 8 MACs/sample); if desired, add a routing predicate `taps_per_phase * up * 4 >= down * 128 → FFT`, but the measured matrix has no such case.

## 6. Risks and validation

| Risk | Mitigation |
|---|---|
| Circular (block-boundary) aliasing if `Nin < nin + Lin` | Static assert at construction: choose `nin = down·s` with `nin ≥ taps_per_phase`, `Nin = 2·nin`. Regression: chunked vs single-feed bitwise identity (existing test pattern in `rubato_backend.rs` `render_backend` with `[127,509,31,1024]` split pattern). |
| Kernel spectrum sampling error (must be FFT of zero-padded time kernel, not analytically sampled response) | Compute `H` from the exact `minimum_phase_prototype` output; add unit test: FFT-engine impulse response ≈ decimated kernel within 1e-12. |
| Energy normalization (`×up` interpolation gain, FFT `1/N` conventions, realfft unnormalized) | Fold `up / (Nin as f64)`-style scale into `H` once at setup; reuse existing dense-magnitude test (`actual_ratio_nonlinear_kernels_preserve_dense_magnitude_response`) against the engine's measured response. |
| Output no longer bit-identical to old polyphase engine | Expected (different arithmetic order). New regression: FFT vs time-domain polyphase max abs error < 1e-9 on the 44.1→48 and 48→96 sine/noise fixtures, plus energy-centroid ordering test (min < linear < max) reused from `polyphase_backend.rs` tests. The 27 quality gates (`listening-nonlinear-*` + quality reports) operate on rendered output characteristics, not bits — they should pass unchanged; run `listening-nonlinear` before/after and archive reports. |
| Drain stall / duration drift | Exact `nin·up/down` per block guarantees pacing; keep `MAX_DRAIN_STALL_ROUNDS`; reuse `finish_extension_covers_the_complete_actual_ratio_kernel`-style test against the new engine. |
| Allocation in hot path via realfft scratch | Preallocate via `make_*_scratch_len()`; extend the `assert_no_alloc` test to a Minimum-phase 44.1→48 case. |
| Mixed-radix FFT perf variance (1176 = 2³·3·7²) | Benchmark both `s` choices (e.g. `nin=588` vs `nin=1176`); rustfft handles 3/5/7 radices well; fall back to larger power-of-two-friendly `s` if p95 regresses. |

New bench evidence to add: rerun `benches/audio_resampler_matrix_perf.rs` (cases already include `upsample_48k_to_96k high minimum stereo 512`) and archive `resampler-matrix-rubato-<after>.json`; add a 44.1→48 Minimum case to the matrix if absent.

## 7. Recommended design (implementable)

**New file** `src/processor/resampler/spectral_nonlinear_backend.rs` (or extend `polyphase_backend.rs` with a second engine), variant `RubatoEngine::SpectralNonlinear` in `rubato_backend.rs`.

Construction (`new(from, to, phase, quality, channels, chunk_frames)`):
1. Reduce ratio, enforce `up, down ≤ 1024` (reuse messages from `polyphase_backend.rs:57`).
2. Design kernel exactly as today: `design_linear_prototype` → `minimum_phase_prototype` → reverse for Maximum → `normalize_kernel`. Keep `latency_frames`/`finish_extension_frames` from the same kernel.
3. Choose `s = max(1, taps_per_phase.div_ceil(down))`, possibly ×2 while `down·s < taps_per_phase` or to reach `nin ∈ [256, 1024]`; `nin = down·s`, `nout = up·s`, `Nin = 2·nin`, `Nout = 2·nout`. Verify `1024 % nin == 0` is NOT required — process ⌈CHUNK_IN/nin⌉ whole blocks and carry the remainder in the internal input staging (engine consumes exactly CHUNK_IN per `process_chunk` call, buffering `< nin` residual frames internally, mirroring how rubato `Fft` uses sub-chunks; simplest: pick `nin` dividing 1024·down-multiples, e.g. force `s` so `nin | CHUNK_IN·k` — for 147:160 use `nin = 588`, buffer residual 1024 mod 588 internally with exact-rational `next_output` pacing identical to today's `process_chunk` accounting).
4. Precompute complex filter spectrum: zero-pad kernel to `Nup = Nin·up` points → FFT (rustfft, setup-only) → retain the `Nout/2+1` bins the output grid needs, folding in the `up/Nin`… normalization and the image-selection map (bin `j` of output grid ↔ bin `j mod (Nin/2+1)`-image of input grid, zero above cutoff `min(nin, nout)`-ish bins). Store per-bin `(source_input_bin, complex_gain)` or a dense complex array aligned to input bins for a branch-free multiply loop.
5. Preallocate per-channel: overlap-save input buffer `[f64; Nin]`, realfft forward (`Nin`) and inverse (`Nout`) plans + scratch, spectrum buffers.

`process_chunk(input, output)` (per adapter chunk of `CHUNK_IN` frames):
- Deinterleave per channel into the staging tail (channels processed sequentially with shared scratch — same pattern as `OverlapSaveConvolver::process_channel_chunk_fft`).
- For each complete `nin` block: shift/copy overlap (`memmove`, contiguous), forward real FFT, complex multiply into output bins, inverse real FFT, write last `nout` samples interleaved to `output` at the exact-rational output cursor.
- Return `(CHUNK_IN, produced)` with `produced` from the same `total_input·up/down` pacing arithmetic as today (guarantees adapter duration accounting unchanged).

`reset()`: zero overlap/staging, reset counters. `output_frames_max()`: `(chunk_frames·up).div_ceil(down) + 2` (unchanged). Adapter changes: route nonlinear phase to the new engine in `RubatoEngine::new`; extend `prefix_budget_direct` to include it (`matches!(engine, Fft(_) | SpectralNonlinear(_))`) once the bit-exact direct-vs-staged tests pass; `latency_frames`/`finish_extension_frames` arms forward to the new engine.

**Expected result**: 48→96 High/Minimum stereo ~5–8 ns/input-sample (≈100× vs 761, beating soxr's 13.8); 44.1→48 Minimum ~7–10 ns; UltraHigh nonlinear drops from ~1.5 µs to ~10 ns class. Implementation complexity: one new ~400–500-line engine file reusing existing kernel design + realfft, no adapter architecture changes, no public API change.

## Related specs

- `.trellis/spec/backend/realtime-safety.md` — hot-path prohibitions; FFT planning setup-only.
- `.trellis/spec/backend/streaming-lifecycle.md` — process/finish progress, duration alignment, reset-to-fresh.
- `.trellis/spec/backend/listening-nonlinear-correctness.md` — nonlinear listening gates to rerun.
- `.trellis/spec/backend/quality-guidelines.md` — quality gate conventions.

## Caveats / Not found

- Flop→ns projections are extrapolated from the measured linear rubato-Fft path (same FFT sizes, same machine), not measured; the recommendation's headroom (100×) makes the conclusion robust to 2–3× estimation error.
- Did not verify rubato 4's internal `Fft` bin-mapping source line-by-line (crate source not vendored); the spectral-replication math is standard (soxr `rate.c`/`dft_filter`, Proakis–Manolakis multirate identity) and independently derivable.
- `MAX_REDUCED_RATE` for nonlinear is 1024 (not 256) — confirmed at `polyphase_backend.rs:16`.
