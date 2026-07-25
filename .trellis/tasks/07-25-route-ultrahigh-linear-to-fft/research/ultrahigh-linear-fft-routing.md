# Research: Route UltraHigh Linear off per-sample sinc — which FFT candidate?

- **Query**: Replace rubato `Async` sinc for UltraHigh+Linear (~91–114 ns @44.1→48, ~154–220 ns @48→96, setup ~5.7–7.6 ms) with an FFT-based engine while keeping UltraHigh the strongest quality tier.
- **Scope**: internal (code + archived same-machine measurement reports)
- **Date**: 2026-07-25

## Current state (facts, with sources)

### Routing — `src/processor/resampler/rubato_backend.rs`

- `should_use_fft` (line 89–97): Linear phase routes to rubato `Fft::new_custom(from, to, CHUNK_IN=1024, FFT_SUB_CHUNKS=2, …, BlackmanHarris2, FixedSync::Input)` for reduced ratios ≤ `MAX_FFT_REDUCED_RATE=1024`, **except UltraHigh**, which falls to `Async` sinc via `sinc_parameters` (line 70–80): UltraHigh = sinc_len 256, oversampling 512, cubic. High 2:1 upsample goes to the dedicated halfband engine.
- Delay handling is engine-agnostic: `initial_delay = engine.output_delay()`, adapter discards that many produced frames at start/after clear (lines 394–400, 448). `Fft` and `Sinc` both use this; `Spectral` reports `output_delay()=0` and instead exposes `latency_frames`/`finish_extension_frames`. So **any engine with a leading-delay contract plugs in without adapter changes**.
- `prefix_budget_direct` (line 434) already covers `Fft` for non-integer ratios — switching UltraHigh from Sinc to Fft would also gain the direct-output fast path.
- Note: rubato `Fft::new_custom` takes **no quality parameter**. Its filter is determined by (rates, chunk, sub_chunks, window). Sub-chunk count changes the FFT unit size and therefore the internal filter length: 1 sub-chunk = 2× longer FFT/filter than 2 sub-chunks.

### Kernel design params — `src/processor/resampler/polyphase_backend.rs`

- `taps_per_phase`: 64/128/256/512 (Low..UltraHigh); `quality_rolloff`: 0.90/0.94/0.96/0.98; `quality_beta`: 8.6/11.0/14.0/17.0 (lines 204–229).
- `design_linear_prototype`: Kaiser-windowed sinc, cutoff `0.5·rolloff/max(up,down)`. Kaiser attenuation formula `A = beta/0.1102 + 8.7` → **beta 17 ⇒ ~163 dB stopband** (not 170).

### Spectral engine — `src/processor/resampler/spectral_backend.rs`

- Rejects Linear explicitly (line 83: `"spectral backend requires a nonlinear phase"`). The overlap-save + exact alias-fold machinery is phase-agnostic; accepting a Linear kernel would mean: skip `minimum_phase_prototype`, use the symmetric prototype directly, report the group delay `(L−1)/2/down` frames as `output_delay()` (adapter delay-skip already generic) instead of `latency_frames`, and drop the `finish_extension` semantics down to the delay-adjusted duration contract (Linear engines produce duration-aligned output after delay skip; today's `latency_frames`/`finish_extension_frames` path is the nonlinear contract).
- Cost structure: fold table has `(nout+1)·down` complex MAC entries per block of `nin = down·s` inputs, i.e. the fold alone costs ~`up` complex MACs per input sample. Cheap at 1:2/2:1, expensive at 147:160 (up=160).

### Quality harness — `benches/audio_quality_measurements.rs`

- Runs `resampler_quality: "UltraHigh"` **hardcoded** (line 891); measures 44.1→48 THD+N (gate −100 dB), 44.1→48 passband deviation 20 Hz–18 kHz, and 96→48 stopband alias attenuation. So every archived quality report measures whatever engine UltraHigh routed to at that revision — which is exactly the comparison we need, and it already exists for all three engine configurations on this same machine:

| Metric (harness, quick) | UltraHigh **sinc** (current; `07-24-specialized-2x…/quality-rubato-halfband2x-final-quick.json`) | rubato **Fft, 2 sub-chunks** (`07-21…/quality-rubato-fft-quick.json`) | rubato **Fft, 1 sub-chunk** (`07-21…/quality-rubato-fft-subchunk1-quick.json`) |
|---|---|---|---|
| THD+N 44.1→48 (dB) | **−216.2** | −200.6 | −204.9 |
| Passband max dev 20 Hz–18 kHz (dB) | 8.2e-10 | 3.1e-11 | **2.0e-11** |
| 20 kHz gain (dB) | −0.00166 | −0.0017 | −0.0017 |
| Worst alias attenuation 96→48 (dB) | −208.1 | −290.2 | **−290.5** |
| Analyzer floor | −296 dB | — | — |

All numbers are 100+ dB below the gate (−100 dB) and below any audibility threshold. The Fft engine **beats** the UltraHigh sinc on stopband (by 82 dB) and passband flatness; sinc wins only THD+N (−216 vs −205), both being ~110 dB below the gate.

### Perf (same machine, quick mode, median ns/input-sample, stereo 512-frame)

From `.trellis/tasks/07-25-optimize-rubato-nonlinear-phase-performance/research/resampler-matrix-rubato-v2-spectral-candidate.json` (current tree) and `07-21…/final-evidence.md` (Fft sub-chunk sweep):

| Case | Sinc (current UltraHigh) | Fft 2-sub (High today) | Fft 1-sub | Spectral engine |
|---|---|---|---|---|
| 44.1→48 Linear | 114.4 (setup 7.6 ms) | 9.9 (setup ~0.16 ms) | 35.1 | High-Minimum measured 191.4 (setup 49.7 ms); Linear UltraHigh est. similar-to-worse (fold ∝ up=160, taps 512) |
| 48→96 Linear | 219.7 (setup 7.5 ms) | 12.6 | 19.2 | High-Minimum measured 15.5 (setup 0.45 ms); UltraHigh Linear est. ~20–30 |
| 96→48 Linear | (routes same) | 6.5 | — | est. ~15–25 |

## Candidate evaluation

### A. Route UltraHigh Linear to rubato `Fft` exactly like High (2 sub-chunks)

- Quality: measured above — identical to High by construction (`Fft::new_custom` ignores quality). 44.1→48 UltraHigh would be **bit-identical to High** (48→96 differs only because High routes to halfband).
- Perf: ~10 ns @44.1→48, ~12.6 @48→96; setup ~0.16 ms. Best raw numbers.
- Risk: **fails the product contract** — UltraHigh would be indistinguishable from High for every common Linear ratio. `ultra_high_preserves_the_sinc_quality_path_for_common_ratios` test documents this as intentional today.
- Verdict: reject as-is.

### A′. Route UltraHigh Linear to rubato `Fft` with **1 sub-chunk** (High keeps 2) ⭐

- The sub-chunk count is a real filter-quality knob: 1 sub-chunk doubles the FFT unit and internal FIR length. Measured (07-21 sweep + subchunk1 quality report): UltraHigh-as-Fft-sub1 is **strictly better than High-as-Fft-sub2 on all three harness metrics** (THD+N −204.9 vs −200.6, passband 2.0e-11 vs 3.1e-11, alias −290.5 vs −290.2) and structurally a 2× longer filter — a defensible "strongest tier".
- vs current sinc: better stopband by 82 dB, better passband by ~40×, THD+N −204.9 vs −216.2 (both ~110 dB below the −100 dB gate and ~90 dB below the analyzer floor's audible relevance). Net: measured quality is equal-or-better where it matters (alias/imaging), microscopically different where it doesn't.
- Perf: **35.1 ns @44.1→48 (3.3× faster), 19.2 ns @48→96 (11× faster)**; setup drops from ~7.6 ms to sub-millisecond. Larger per-chunk latency smoothing (one 1024-frame FFT unit) is internal; delay handled by the existing `output_delay()` skip; `prefix_budget_direct` applies unchanged.
- Implementation: ~10 lines — `should_use_fft` drops the UltraHigh exclusion; `FFT_SUB_CHUNKS` becomes quality-dependent (`UltraHigh → 1`, else 2); update the two routing tests; rerun quality harness + matrix bench for evidence.
- Risk: low. The 07-21 sweep rejected sub-chunk 1 for *High* on perf grounds ("misses the 44.1→48 target"), not quality — for UltraHigh, spending 35 ns for the longest filter is exactly the tier's meaning.

### B. Extend the spectral engine to Linear phase (UltraHigh prototype: 512 taps/phase, beta 17)

- Quality is bounded by the Kaiser design: beta 17 ⇒ **~163 dB stopband**, rolloff 0.98. That is *weaker* than both the current sinc's measured −208 dB and Fft's −290 dB alias attenuation — the harness stopband number would regress ~45 dB. Passband ripple fine.
- Perf: excellent only for small reduced ratios. Measured proxy: 48→96 High-Minimum spectral = 15.5 ns. But 44.1→48 (up=160) High-Minimum measured **191 ns** — the exact alias fold costs ~`up` complex MACs/input-sample and taps 512 doubles block sizes; UltraHigh Linear at 44.1→48 would land ~200–400 ns, **worse than today's 114 ns sinc**. Setup at 44.1→48 measured 49.7 ms (Nup=~94k FFT; linear skips the cepstrum, so somewhat less, still tens of ms).
- Integration cost: lift the Linear rejection, add symmetric-kernel `output_delay() = ((L−1)/2)/down` plumbing (delay-skip exists, but the Spectral arm currently reports delay 0 and uses the nonlinear latency/extension contract — needs a per-phase split), plus new bit-exactness/duration tests.
- Verdict: reject — loses on quality (163 dB design ceiling), loses on perf at the flagship 44.1→48 ratio, and is the most code.

### C. Hybrid: longer half-band cascade for 1:2/2:1, spectral-linear (B) elsewhere

- Half-band leg is fine (~5 ns class, and a longer UltraHigh halfband could exceed the current −208 dB), but "elsewhere" is exactly the 147:160 case where B fails (see above). Two new engines + per-ratio routing for a result A′ matches or beats with one constant change.
- Verdict: reject.

## Recommendation

**Adopt A′: route UltraHigh+Linear to rubato `Fft` with 1 sub-chunk, keeping High at 2 sub-chunks.** It is the only candidate that simultaneously (1) beats High on every measured quality gate metric — preserving "UltraHigh is strongest" with archivable evidence, not just semantics; (2) improves on the current sinc's worst metric (stopband, +82 dB) while all metrics stay >100 dB below gates; (3) cuts 44.1→48 from 114→35 ns and 48→96 from 220→19 ns with setup dropping ~30×; (4) needs only a quality-conditional constant plus test updates, zero adapter/delay changes.

Verification plan for the implementer: change `should_use_fft` + sub-chunk selection; rerun `cargo bench --bench audio_quality_measurements -- --quick --enforce --out …` (harness is hardcoded to UltraHigh, so it directly measures the new route) and `audio_resampler_matrix_perf` quick mode; expect ≈ the archived subchunk1 numbers; update `ultra_high_preserves_the_sinc_quality_path_for_common_ratios` (rename: UltraHigh now expects `Fft`) and keep the 44_100→44_101 pathological-ratio sinc fallback assertion (UltraHigh still needs the sinc fallback for reduced ratios > 1024 — keep `sinc_parameters`' UltraHigh row).

## Related specs

- `.trellis/spec/backend/quality-guidelines.md` — gate conventions; archive before/after quality JSON.
- `.trellis/spec/backend/realtime-safety.md` — Fft engine already satisfies (same as High path).
- `.trellis/spec/backend/streaming-lifecycle.md` — duration alignment; `prefix_budget_direct` tests already cover the Fft engine.

## Caveats

- The subchunk1 quality/perf numbers are from the 07-21 revision (same machine); rerun after the change for current-tree evidence. Perf numbers are quick-mode medians with visible run-to-run jitter (±20%).
- Rubato `Fft`'s filter length also scales with the reduced ratio; the sub-chunk quality ordering was verified at 44.1→48 and 96→48 (harness ratios) but not at every matrix ratio — the structural argument (2× FFT unit ⇒ 2× filter) holds for all.
- Did not run new measurements in this session; all numbers are from archived same-machine reports (paths cited inline).
