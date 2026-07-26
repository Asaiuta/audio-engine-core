# Research: Making large-`down` rational nonlinear ratios (147:160) fast

- **Query**: Spectral nonlinear engine costs ~191 ns/input-sample (bench box) for 44.1k→48k High/Minimum vs 15.5 ns at 1:2 and soxr's 10.4. How to speed up large-`down` ratios without losing the exactness/quality contracts (<1e-9 FFT-vs-polyphase parity, unchanged kernel/latency/finish semantics, realtime safety)?
- **Scope**: internal (code study + empirical probes run via temporary `cargo test --release` probe, reverted) + analysis
- **Date**: 2026-07-25

## 1. Where the 191 ns actually goes (measured)

Geometry for 44.1k→48k High (`up=160, down=147`, `taps_per_phase=256`):
`s = ceil(256/147) = 2` → `nin = 294`, `nout = 320`, `Nin = 588`, `Nout = 640`,
`fold.len() = (nout+1)·down = 321·147 = 47 187` entries per block per channel
(`spectral_backend.rs:121-175`).

Temporary probe (added inside `mod probe` in `spectral_backend.rs`, run with
`RUSTFLAGS="-C debug-assertions=on" cargo test --release --lib --no-default-features --features rubato`,
file reverted afterwards; this machine ≈ 1.7× slower than the bench box, scale ×0.59
to compare with the 191 ns report):

| Component | measured (this box) | share | scaled to bench box |
|---|---|---|---|
| full engine, stereo | **326 ns/input-frame** | 100 % | ~191 (matches report) |
| fold loop only | **290 ns/input-frame** | **89 %** | ~170 |
| fwd(588)+inv(640) real FFT | 3 300 ns/block → 22 ns/frame | 7 % | ~13 |
| copies/staging | ~14 ns/frame | 4 % | ~8 |

**The fold is essentially all of the cost, and it is memory-bandwidth-bound, not
flop-bound.** The `FoldEntry` table is 47 187 × 32 B ≈ 1.5 MB streamed once per
294-frame block per channel (≈ 10 KB per input frame stereo). A structure-of-arrays
rewrite (dense split-re/im H, contiguous per-alias-image passes, no index/sign
fields, ideal SIMD shape) was probed and only reached **241 ns/frame** (vs 290) —
because it still streams 754 KB of `H` per block per channel, which exceeds L2 and
runs at the measured ~21 GB/s. Per-entry cost ~0.9 ns scalar is already near the
bandwidth wall.

Key structural fact: the exact alias fold costs `up` complex MACs **per input
sample per channel** regardless of `s` (entries/frame = `(nout+1)·down/nin ≈ up`).
Larger `s` only amortizes the (already small) FFT. So candidate A ("plan the fold
better / bigger s") has a hard floor around ~150–240 ns/frame for `up = 160`;
`f32` H would halve bandwidth but injects ~1e-7 relative kernel error → breaks the
1e-9 parity gate (High-quality Kaiser β=14 stopband is only ~-136 dB ≈ 1.6e-7, so
threshold-pruning near-zero H terms also breaks parity — there are no droppable
terms below 1e-9).

## 2. Candidate comparison (147:160 High Minimum, stereo, ns per input frame)

| Candidate | measured/estimated (this box) | scaled to bench box | exactness | complexity |
|---|---|---|---|---|
| Current spectral fold | 326 (measured) | 191 | exact | — |
| A1: SoA/dense fold, per-image contiguous passes | 241 fold + 22 FFT ≈ 265 (probed) | ~155 | exact | low |
| A2: f32 H or pruned fold | ~130–180 | ~80–105 | **breaks 1e-9 parity** | low |
| B: two-stage spectral 44.1k →(8:7)→ 50.4k →(20:21)→ 48k | est ~55–70 | ~35–45 | **breaks single-kernel parity oracle**; needs two-stage oracle + quality re-evidence, composed latency/finish | high |
| **C: contiguous time-domain polyphase (unrolled, planar history, shared coeff loads across channels)** | **68 (probed: 62.7 ns/output × 160/147)** | **~40** | **exact; bit-parity with oracle achievable** | **low-medium** |
| Naive contiguous polyphase (no unroll) | 122 (probed) | ~72 | exact | low |

Probe details for C: 256-tap f64 dot product, 4 accumulators, both channels
computed in the same pass over one coefficient slice (coefficients loaded once for
L and R) → 62.7 ns per stereo output. The 160×256×8 B = **327 KB coefficient bank
fits L2** (unlike the 754 KB fold spectrum), which is exactly why time domain wins
here: per input frame it streams 4.4 KB/ch of coefficients from L2 vs the fold's
5.1 KB/ch from L3/DRAM, and needs only `taps·up/down ≈ 279` real MACs/frame/ch vs
the fold's `up = 160` complex MACs (≈ 1 100 flops)/frame/ch. Note the default
x86-64 build baseline is SSE2 (no `target-cpu=native` in this repo), so 0.12
ns/MAC from the unrolled loop is realistic, not AVX-optimistic.

Why not two-stage (B), despite decent numbers: 147 = 3·7², 160 = 2⁵·5. Best split
is (8:7)·(20:21) with intermediate 50.4 kHz (up-first keeps every intermediate
Nyquist above the single-stage 21.17 kHz passband edge; any down-first split, e.g.
(16:21)·(10:7) via 33.6 kHz, truncates the passband and is disqualified). Product
of two minimum-phase kernels is minimum-phase, so phase semantics survive, but:
the overall response ≠ the single kernel `design_linear_prototype(160,147,256,High)`
→ the existing spectral-vs-polyphase 1e-9 test cannot be reused as-is; latency =
sum of per-stage `phase_peak_latency_frames` (no longer the documented single-kernel
formula); finish extension composes; all 27 nonlinear listening/quality gates need
re-evidence for a genuinely different filter. Estimated win over C is ~0–10 ns —
not worth the contract churn.

soxr (D): its 10.4 ns comes from cascaded ×2 DFT half-band stages plus a short
variable-rate interpolated-coefficient polyphase stage — coefficients are
*interpolated between phases*, i.e. it never computes the exact rational kernel
and has no bit-parity contract. Not adoptable under this repo's exactness gates;
its lesson (do heavy anti-alias filtering at a cheap 2× stage, keep the rational
part short) is what candidate B encodes, already rejected above.

## 3. Recommended design: contiguous polyphase engine for large-`up` ratios

Revive the time-domain engine as a production path with a cache/SIMD-friendly
layout (the retired `PolyphaseResampler` in `polyphase_backend.rs` is `cfg(test)`
and slow only because of its per-tap `%` ring indexing and interleaved history,
`polyphase_backend.rs:169-177`).

**New engine** (e.g. `contiguous_polyphase_backend.rs`, or a rewrite inside
`polyphase_backend.rs` promoted out of `cfg(test)`):

- Setup (allocation allowed): identical kernel pipeline —
  `design_linear_prototype → minimum_phase_prototype → reverse for Maximum →
  normalize_kernel → polyphase_coefficients(kernel, up, taps_per_phase)`; identical
  `latency_frames = phase_peak_latency_frames(kernel, down)`,
  `finish_extension_frames = (L-1).div_ceil(down)`, `output_delay() = 0`,
  `output_frames_max = (chunk·up).div_ceil(down)+2`, `MAX_REDUCED_RATE`/
  `MAX_POLYPHASE_COEFFICIENTS` checks unchanged.
- Data flow per `process_chunk` (adapter feeds fixed `CHUNK_IN = 1024` frames):
  1. Per channel **planar** history buffer of `taps_per_phase - 1 + CHUNK_IN`
     f64 (High stereo: 2 × (255+1024) × 8 B ≈ 20 KB). One `copy_within` moves the
     last `taps_per_phase - 1` samples to the front, then the chunk is
     deinterleaved into the tail. No modulo anywhere.
  2. Same exact-rational pacing arithmetic as today
     (`next_output < total_input·up/down`, `polyphase_backend.rs:156-164`); for
     each output: `phase = (next_output·down) % up`, `base` index into the planar
     buffer, then one **contiguous** `taps_per_phase` dot product per channel with
     4 accumulators, iterating channels in the inner pass so each coefficient
     slice is loaded once (the probed shape).
  3. Write interleaved output; return `(1024, produced)` — same contract, so the
     adapter's `prefix_budget_direct` eligibility (`rubato_backend.rs:445`) carries
     over by adding the new variant next to `Fft | Spectral`.
- Per-call bounds: ≤ `(1024·up).div_ceil(down)+1` outputs × `taps_per_phase·channels`
  MACs; no data-dependent loops, no allocation (all buffers in `new`), satisfies
  `.trellis/spec/backend/realtime-safety.md`.
- `reset()`: zero history, reset `total_input/next_output/pending` — reset-to-fresh
  bit-exactness is trivial (pure feed-forward FIR).

**Routing rule** (in `RubatoEngine::new`, `rubato_backend.rs:243`): keep the
spectral engine where it wins (FFT-dominated, tiny fold) and route large-`up`
ratios to the contiguous polyphase engine. Cost model per input frame per channel:
spectral ≈ `0.9·up + FFT(~11 ns)`, polyphase ≈ `0.13·taps_per_phase·up/down`.
A simple, safe threshold that matches both measured endpoints:

```
nonlinear phase:
    if up <= 16  -> SpectralNonlinearResampler   (1:2, 2:1, 2:3, 3:4, ... families)
    else         -> ContiguousPolyphaseResampler (147:160, 160:147, 320:147, ...)
```

(`up ≤ 16` keeps every case where the fold is ≤ ~16 complex MACs/sample, i.e.
spectral ≈ 10–20 ns; at `up = 16, down = 15` the two paths are roughly tied, so
the exact threshold is not delicate. Both paths keep `up, down ≤ MAX_REDUCED_RATE`.)

**Expected results (bench box, stereo, ns/input-sample)**: 44.1k→48k High Minimum
191 → **~40** (UltraHigh ~×2 → ~80; Standard ~×0.5 → ~20); 48k→96k stays on
spectral at 15.5; 48k→44.1k (up=147) ~35–40; 44.1k→96k (up=320, down=147)
~70–75 (vs ~350+ on spectral today).

## 4. Required evidence / regressions

- **Parity**: the existing `spectral_output_matches_time_domain_polyphase_within_1e_minus_9`
  test keeps the `cfg(test)` oracle as-is; add the same 1e-9 comparison for the new
  contiguous engine (it should in fact be reachable to ~1e-15 or bit-identical if
  the dot-product accumulation order is kept deterministic — the 4-accumulator
  split changes rounding vs the oracle's serial sum, so target <1e-9, and add a
  serial-sum debug check if bit-parity is desired).
- Timing metadata test (latency/finish vs polyphase formulas) extended to the new
  engine; adapter tests: chunked-vs-single-feed bitwise identity, `assert_no_alloc`
  on a 44.1→48 Minimum case, clear-after-partial-drain, drain duration alignment.
- Bench: rerun `benches/audio_resampler_matrix_perf.rs`, archive before/after
  matrix JSON; confirm no regression on the small-`up` cases still routed spectral.
- Quality gates: kernel is unchanged bit-for-bit, so `listening-nonlinear-*` and
  the dense-magnitude tests must pass unchanged — run once to archive evidence.
- Routing unit test mirroring `should_use_fft`-style tests: asserts 44.1↔48 routes
  polyphase and 48↔96 routes spectral for all nonlinear qualities.

## 5. Rejected alternatives (documented for the record)

- **A (fold optimization)**: probed SoA/dense-H fold gives only 326→265 ns; the
  fold is bandwidth-bound on an irreducibly `up`-sized, 754 KB spectrum table.
  f32/pruning break the 1e-9 gate (stopband floor 1.6e-7 at β=14).
- **B (two-stage spectral (8:7)·(20:21) via 50.4 kHz)**: est ~35–45 ns, marginally
  better than C at best, but replaces the single designed kernel (new oracle, new
  latency/finish composition, full quality re-evidence, passband-edge bookkeeping).
  Revisit only if C's ~40 ns proves insufficient.
- **D (soxr-style interpolated polyphase)**: incompatible with exact-kernel parity.

## Files studied

- `src/processor/resampler/spectral_backend.rs` — fold construction 121–175, hot loop 280–291
- `src/processor/resampler/polyphase_backend.rs` — oracle (cfg(test)) 130–186; kernel design 231–256; taps/beta tables 204–229
- `src/processor/resampler/rubato_backend.rs` — routing 235–261, direct-path gate 445
- `.trellis/tasks/07-25-optimize-rubato-nonlinear-phase-performance/research/fft-nonlinear-phase-design.md` — prior design; its §2(c) "contiguous polyphase fallback" prediction (~80–150 ns) is confirmed and beaten (68 ns probed) by the shared-coefficient-load stereo layout.

## Caveats

- Probe box is ~1.7× slower than the bench box; all "scaled" numbers use the
  326/191 ratio from the full-engine measurement. Probe was run with
  `-C debug-assertions=on` (needed because `assert_no_alloc`'s `AllocDisabler` is
  compiled out in plain release), adding small overhead — real release numbers
  should be slightly better.
- The 62.7 ns/output dot-product probe uses synthetic strides that touch all 160
  phases; real access order is sequential in phase, if anything friendlier.
- UltraHigh at up=160 has a 655 KB coefficient bank (> typical 512 KB L2); expect
  ~2.2–2.5× the High cost rather than exactly 2×.

## 6. Implemented result and acceptance evidence (2026-07-26)

The production implementation follows candidate C with one refinement: the
outer `RubatoEngine` keeps four top-level branches and nests the two nonlinear
engines under `NonlinearEngine`. This isolates Linear dispatch from the new
backend while preserving one setup-selected nonlinear engine. The contiguous
stereo dot product selects AVX2 during construction, uses multiply plus add
(no FMA), and is bit-equal to the scalar four-accumulator reduction for 4, 13,
256, and 513 taps.

### Accepted heavy comparison

Both executables were pinned to logical core 2 (`affinity mask 0x4`) with High
process priority. Reports identify the same base revision
`1798833cc18678af3537080e63d5b45edb59c40c`; baseline records `dirty=false`,
candidate records `dirty=true`. The accepted matrix comparison uses four
baseline and four candidate heavy runs and the median of each run's case
median (for four values, the mean of the middle pair):

| Case | v3 spectral baseline | v4 hybrid candidate | Result |
|---|---:|---:|---:|
| 44.1→48 High/Minimum stereo 512 | 137.72 ns/input sample | 27.53 ns/input sample | **5.00x faster** |
| 48→96 High/Minimum stereo 512 | 10.60 | 10.36 | 2.28% faster |
| 48→96 Standard/Minimum stereo 512 | 9.52 | 9.58 | 0.67% slower |
| 48→96 UltraHigh/Minimum stereo 512 | 11.82 | 11.94 | 1.01% slower |

Accepted full-matrix files:

- `matrix-baseline-v3-heavy-pinned-nested-b1.json` through `-b4.json`
- `matrix-candidate-v4-heavy-pinned-nested-c1.json` through `-c4.json`

The short whole-matrix timer reported one real Linear conversion just outside
the 5% retention gate: 48→192 High/Linear, six channels, 512 frames at +5.96%.
That case costs only about 19 ns/sample and each heavy trial covered 600
buffers, so a focused 6000-iteration × 21-trial B-C-C-B run was used rather
than weakening the gate. Its averaged medians were 19.526 ns baseline and
18.872 ns candidate, **3.35% faster**. Evidence is in
`matrix-{baseline-v3,candidate-v4}-linear-retention-pinned-{b1,b2,c1,c2}.json`.
Equal-rate bypass rows near 0.08 ns/sample remain timer-quantized and are not
evidence about a resampling engine.

One earlier candidate run had a visibly bimodal unchanged half-band case
(roughly 4.5 ns versus 8.6–13.3 ns trials). It is retained as
`matrix-candidate-v4-heavy-pinned-formal-c2.json` and excluded as host-load
contamination, consistent with the benchmark evidence policy. A later boxed
nonlinear-state experiment caused repeatable 5–10% Linear regressions; its
`*-boxed-*.json` reports are retained and the code was reverted.

### Correctness and quality

- Complete Rubato suite: 387 library tests, 18 benchmark-support tests,
  3 Windows runtime tests, and 2 doctests passed.
- Both feature configurations passed `cargo check`; both strict Clippy matrices
  passed with `-D warnings`; task-owned files were formatted with rustfmt.
- `matrix-candidate-v4-quick-final.json` passed `--quick --enforce` and records
  the final `matrix_process_checked_v4_nonlinear_polyphase_up16` identity.
- `quality-candidate-v4-quick-final.json` passed all 27/27 gates. Representative
  resampler evidence is THD+N -204.95 dB, effectively 0 dB passband error, and
  worst alias attenuation -290.48 dB.

The acceptance result is therefore retain: large-`up` nonlinear ratios use the
contiguous engine, small-`up` nonlinear ratios remain spectral, and Linear
routing is unchanged.
