# Review Findings 2026-08-11 — Resampler

Source: sample-rate-conversion deep-review agent report from the 2026-08-11
six-track review. Both backend test suites were executed during review
(rubato feature: 75 pass; default soxr: 23 pass).

## Headline verdict

No triggerable correctness defect in shipped execution paths. Independently
re-derived and confirmed: 127-tap Kaiser (β=14) half-band odd/even polyphase
decomposition (center-tap-only odd phase, 31-frame direct delay, output
delay 63 matching the kernel center); spectral overlap-save alignment and
the Hermitian half-spectrum alias-fold
`Y[k]=(1/down)·Σ H·X_ext` with `up/(down·Nout_full)` scaling folded exactly
once; real-cepstrum minimum phase (fold, ×2 positive quefrency, exp) with
4× zero-padding; contiguous polyphase `q=n·down` index mapping and history
head `taps-1+ceil((down-1)/up)` lower-bound proof. Cursors are exact
u64/u128 integer rationals — no float drift on the FFT/nonlinear routes.
SIMD kernels are bit-equal to scalar by constructed reduction order, locked
by tests.

## Findings (all defense-in-depth)

1. **(med-low)** `soxr_backend.rs:159-165` — `latency_frames()` /
   `finish_extension_frames()` hardcoded 0; relies on libsoxr's
   duration-aligned drain rather than `soxr_delay()`. Behavior pinned by
   `native_drain_returns_a_duration_aligned_impulse_sequence`; a different
   libsoxr build breaks the test first (good) but the reliance is
   undocumented. Mid-stream pipeline latency deliberately unexposed —
   document the timeline-vs-throughput-delay distinction.
2. **(med-low)** `mod.rs:1288-1300` — `drain_into_interleaved` treats a
   partially filled output as flush-complete; `drain_into_mono`
   (`:1169-1259`) uses explicit zero-return rounds. Verified safe for both
   current backends (rubato partial ⇒ `emitted == expected_total`; libsoxr
   flush front-loads its FIFO) but the invariant is unwritten. Unify on
   zero-return confirmation.
3. **(med, hard-realtime narrative only)** libsoxr C-heap allocations
   (fifo.h realloc on demand in `soxr_process`) are outside
   `assert_no_alloc`'s Rust-allocator hook: first call or larger chunks
   allocate on the "realtime" path; steady after the 16384-frame cap. The
   no-alloc guarantee is strict only for the pure-Rust backend — document
   the per-backend strength.
4. **(low)** `rubato_backend.rs:819-829` — sinc fallback
   (`Async::new_sinc(to/from as f64)`) carries f64 phase; long pathological
   streams see sub-sample drift; rational `expected_total` (+`.max(emitted)`
   guard) bounds duration error to ±1 frame. Accepted bound; note it.
5. **(low)** `polyphase_backend.rs:318-330` — `phase_peak_latency_frames`
   uses kernel-peak position as the latency scalar; min-phase group delay is
   frequency-dependent (Minimum reports ≈0, Maximum ≈full length;
   latency+tail exactly partitions the extension — test-locked). Fine for
   gapless; caveat needed for sample-exact A/B alignment users.
6. **(low)** `mod.rs:443-579` — one-shot `resample_parallel` with
   Minimum/Maximum yields duration+latency+tail output (Linear yields exact
   duration); undocumented, and `converted_output_frames` under-reserves by
   the extension (one extra Vec growth).
7. **(low)** `soxr_backend.rs:33-36` — no `SOXR_DOUBLE_PRECISION` on
   Low/Standard/High (≤20-bit recipes run f32 internally despite f64 I/O);
   UltraHigh (Bits28) auto-double. Recipe-conformant; clashes with "f64
   Hi-Fi" phrasing — flag or document.

## Dependency (upstream soxr 0.6.0) watch items

- `params.rs:238` — `coef_size_kbytes()` getter returns
  `log2_large_dft_size` (copy-paste bug). Not called by this crate.
- `raw.rs:16` — `unsafe impl Sync for SoxrPtr` without internal
  synchronization; unexploitable here (all mutation `&mut self`,
  single-threaded instances) but aggressive. Re-audit on upgrade.

## Style notes routed to `08-11-style-docs-cleanup`

`rubato_backend.rs:1774` bare `as` cast (vs `usize::try_from` at `:1237`);
dead length-mismatch zero-fill branches in
`interleave_channel_outputs_to_*` (`mod.rs:357-363`, `:395-404`);
"SoX VHQ" used as a blanket backend label while default High maps to
Bits20/HQ; heavy `#[cfg(feature)]` double-branching readability;
`should_use_fft`'s unused `_quality`; `nonlinear_uses_spectral` returning
`true` for zero rate to route the error through the constructor.
