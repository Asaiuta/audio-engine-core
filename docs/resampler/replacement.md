# Owned Rust resampler replacement

The later [fused-FFT optimization and resource report](resources.md)
describes the current inverse-transform implementation. The replacement and
full-waveform replay evidence below identifies the preceding O8 integration;
it was not rerun or relabeled for that local optimization.

The default `rubato` Cargo feature selects the crate's owned Rust backend,
whose display name is **CoreSinc** and whose stable machine identifier remains
`rust`. Existing consumer feature selections continue to work. The machine
identifier is deliberately unchanged so benchmark records and consumers do not
need a compatibility migration just for the display name.
Production code no longer depends on upstream Rubato or directly on
`sync_wrapper`. Upstream Rubato 4.0.0 remains optional under `raw-rubato`, solely
for benchmark controls; the default `rubato` feature does not enable it.
When `soxr` is also enabled, SoXR still takes precedence.

## Contract and implementation

The public constructors, typed errors, phase/quality choices, consumed/produced
counts, rounded finite output duration, finish/reset behavior and public auto
traits are preserved. Ordinary processing uses storage allocated during setup
and performs no heap allocation, deallocation, locking or logging.

Common linear-phase ratios use owned FFT units. Large reduced ratios use an
output-driven sinc history and the former backend's virtual floating-point
clock. Exact 2x High upsampling and nonlinear-phase conversion keep their
existing half-band and FIR kernels. Nonlinear phase retains its checked
reduced-geometry limit. The former prototype's channel/ratio caps are not
public API limits; allocation geometry and counters are checked.

The filter designs, normalization, interpolation/reduction ordering and delay
conventions are adapted from Rubato 4.0.0 under its MIT license, reproduced in
[NOTICE](../../NOTICE). This is an owned implementation of the consumed backend
functionality. Sample compatibility preserves the previous fractional timeline
offset and does not establish corrected absolute origin or zero device latency.

## Recorded replacement evidence

The source-bound optimization-o8 candidate was compared with the captured
production reference at `391d80715dbce4e1fea22fe293caee95b2b1e7d6` on Windows/x86_64.

| Check | Result |
| --- | --- |
| Complete C1 waveform pairs | 12,336 / 12,336 passed |
| Complete compared output samples | 685,414,488; zero differing bits |
| Performance population | All 828 rate/quality/channel/phase configurations |
| Primary plus development guard keys | 861 / 861 passed |
| Identical-implementation controls | 54 / 54 passed |
| Population geometric candidate/reference cycle ratio | 0.921156058 |
| Largest primary overall or construction-stratum ratio | 1.041773067 |
| Control overall and construction-stratum range | 0.985007038 to 1.016758747 |
| Accumulated child measurement time, including reference dose | 496.056 seconds |
| Hot-path heap operations, retained-heap/startup regressions | None in the accepted measurements |

The performance identity is `l5_retest_v1`. It measures Windows thread
CPU cycles on logical processor 8 of an Intel Core i5-12500H, with affinity
mask 256 and above-normal child priority. Wall durations are retained separately.
Each population configuration has one prespecified chunk from 64, 256 and 1024
frames. Extra guards do not contribute to the population mean. Four initial
layouts receive exactly one twelve-layout supplement when the frozen precision
rule requires it; all observations and both construction orders remain included.
The control, primary and population gates are respectively 3%, 5% and 0.95.

This is bounded evidence for the selected workload and processor. It is not a
full Cartesian chunk comparison, T4 wall-time acceptance, a device deadline
guarantee, or a universal speedup. T4 was stopped at the user's request; L2/L3
calibration failures and L4's interleaved-control failure remain retained.
The original R5 108-configuration tone suite is historical evidence, not a
tone rerun for later optimization identities. The later complete waveform
replays provide their recorded numerical compatibility evidence.

The frozen research for this replacement is local to the development machine and
deliberately not distributed with this repository. The acceptance decision SHA-256 is
`32cf377ef84c1e502d59c1a2853dc022eb5cc7a1c54c0ec5b48403a1aec45083` and its binary SHA-256 is
`addccc75664ce04be34ff33c268571c6c57605a5c6e3d38ee31f8e5c76ac6cbf`.

The 2026-10-10 `comparison-repair-v1/root-replay-v3` additionally compiled the
unchanged C1 evaluator against the actual root package with isolated `rubato`.
All 12,336 waveform pairs and 685,414,488 output samples again matched bit for
bit. All 828 operational pairs passed, with zero hot-path heap operations,
828 retained-heap improvements and 267 better / 561 equal startup observations.
All three child processes exited 0 and captured source hashes stayed unchanged.
Three captured source paths differed from O8 only in line endings; this is
source-text equivalence, not byte-identical recovery. No new tone or historical
L5 timing run is implied. The actual-root replay executable SHA-256 is
`92d78711645b85685e48af119932a277a6986d8d474c1e7202e7c6e0c69f221d`.

## Repository checks and reporting

The integration verification matrix covers default, isolated `rubato`, and all-feature builds,
public API snapshots and minor-policy SemVer, MSRV 1.87, documentation, the
isolated AutoMix consumer, and package contents. Source-bound command receipts
are retained in the versioned `comparison-repair-v1/final-validation-*/` folders.
The package check uses a current-byte mirror containing
tracked files and owned additions, excluding unrelated untracked AutoMix work.
Its receipts and archive are retained in `package-verification-*/`.
Portable-path tests and available cross-target compilation do not imply native
ARM or Linux execution coverage.

Current benchmark reports identify the backend as `rust`. The comparison runner
has eleven engine IDs, including the optional `raw_rubato` control and its
`--raw-rubato-geometry` selector. The repaired v7 reports retain engine identity
on each trial and separately measure frequency response, alias/image rejection,
channel isolation and process-call tails. The two October comparison runs cover
48 Rust-build and 52 SoXR-build cases; their scope and the withdrawn v6 summaries
are documented in [comparison.md](comparison.md).

A future algorithm may depart from Rubato sample equality if its frozen paired
objective quality, bandwidth, phase and complete-stream transient criteria do
not regress. The current implementation still preserves its recorded sample
compatibility; no new filter was introduced by the comparison repair.
