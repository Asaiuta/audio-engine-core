# Cache FFT plans instead of rebuilding planners per call

## Goal

`FftPlanner::new()` returns an *empty* cache. Building a plan then costs a prime
factorization, algorithm selection, and twiddle-factor precomputation. Several
call sites construct a fresh planner on every call, so that setup work is
recomputed and thrown away each time.

Measured on this host at n = 8192: `plan_fft_inverse` costs **171.3 us** from a
fresh planner versus **0.072 us** from a warm one — a ~2400x difference on the
planning step alone. Upstream documents this explicitly: "If this is called
multiple times, the planner will attempt to re-use internal data between calls,
reducing memory usage and FFT initialization time." We deny it that chance.

## Survey: every `*Planner::new()` in `src/`

| Site | Function | Called | Verdict |
| --- | --- | --- | --- |
| `fir_design.rs:52` | `minimum_phase_from_log_magnitude` | every call | **fix** |
| `polyphase_backend.rs:280` | `minimum_phase_prototype` | every call | **fix** |
| `fir_eq.rs:229` | `generate_linear_phase_ir` | every `regenerate_ir` | **fix** |
| `fir_eq.rs:386` | test oracle | test only | leave |
| `spectral_backend.rs:146` | `SpectralNonlinearResampler::new` | once per object | leave |
| `spectral_backend.rs:185` | `SpectralNonlinearResampler::new` | once per object | leave |
| `convolver.rs:336` | `OverlapSaveConvolver::new` | once per object | leave |
| `convolver.rs:728` | `PartitionedConvolver::new` | once per object | leave |
| `spectrum.rs:51` | `SpectrumAnalyzer::new` | once per object | leave |
| `spectrum.rs:291` | test oracle | test only | leave |
| `polyphase_backend.rs:346` | test helper | test only | leave |

The `new()`-scoped sites already store their plans in struct fields, so each
object pays once. They are correct as written and are **out of scope**. The
problem is confined to free functions that are called repeatedly.

## Why this matters more than it first appears

`minimum_phase_prototype` and `minimum_phase_from_log_magnitude` each build their
own planner, so a single resampler setup plans the **same 8192-point transform
twice**, from two cold caches. A planner also shares recipes and twiddles across
directions: cold `inverse(8192)` is 392 us, and a `forward(8192)` immediately
after is 271 us rather than another 392 us. Splitting the planners forfeits that.

More importantly, `FirEq::regenerate_ir` is **not** a one-time setup path. It is
called from six places, including `set_band`, `set_bands`, `set_sample_rate`,
`set_num_taps`, and `set_phase_mode` — i.e. every time a user moves an EQ slider.
Measured `set_band` cost today:

| taps | Linear | Minimum |
| ---: | ---: | ---: |
| 255 | 37.4 us | 148.7 us |
| 511 | 61.1 us | 297.5 us |
| 1023 | 126.7 us | **517.2 us** |

A 517 us cost per slider movement is a visible interaction cost, and most of it
is planning work that is identical every time.

## Measured ceiling

Simulating the real 48k->192k UltraHigh minphase chain (two fresh planners per
call vs one shared warm planner):

| | per call |
| --- | ---: |
| current (2 fresh planners) | 1540.9 us |
| one shared warm planner | 721.7 us |
| **saving** | **819.2 us (53%)** |

Against the measured real setup total of 1454 us for that configuration, this
projects to roughly **635 us, a ~56% end-to-end reduction**.

Note: the earlier 1149-4648 us setup figures in this session were measured with
`--all-features`, which enables the default `soxr` backend and therefore never
entered this code path. All numbers here come from
`--no-default-features --features rubato`.

## Decision (ADR-lite)

**Decision.** Thread a reusable plan cache through the affected free functions
rather than introducing a global or thread-local cache.

**Rejected: a global/`thread_local` planner.** `FirEq::new(f64, usize)` is public
API and `num_taps` is caller-controlled with no upper bound, so a process-wide
cache keyed by FFT size is an unbounded, caller-driven memory growth path in a
long-running audio process. A per-owner cache is bounded by the owner's lifetime.

**Shape.** Add a small internal plan-cache type in `fir_design.rs` that owns an
`FftPlanner<f64>` plus the plans it has handed out, and pass `&mut` to the
functions that need it. `FirEq` holds one as a field so consecutive
`regenerate_ir` calls reuse it. The resampler backends create one per
construction and share it between `minimum_phase_prototype` and
`minimum_phase_from_log_magnitude`, collapsing today's two cold planners into one.

**Correctness.** A plan is a read-only object (algorithm choice plus twiddle
tables); `process` does not mutate it. A probe confirmed a cache-hit plan
produces **bit-identical** output to a freshly planned one (`rel maxdiff = 0.0`),
so no test tolerance may be relaxed to accommodate this change. Any numeric drift
would mean the refactor changed transform geometry, not that caching is lossy.

**Risks.**

- `FirEq`'s committed public-API baseline includes `impl Send`/`impl Sync`, both
  auto-derived. `rustfft::Fft` is declared `Sync + Send`, so `Arc<dyn Fft<f64>>`
  should preserve them — but this must be *verified* against
  `tests/public_api.rs`, not assumed. This is the same class of failure as the
  earlier unwind-safety widening.
- Adding a field to `FirEq` changes its size and drops any implicit `Copy`-like
  cheapness; `FirEq` has no `derive`s, so this is expected to be inert.
- `minimum_phase_from_log_magnitude` is `pub(crate)` and
  `minimum_phase_prototype` is `pub(super)`, so signature changes stay internal.

## Scope

**In scope**

- Internal plan-cache type in `src/processor/fir_design.rs`
- `minimum_phase_from_log_magnitude`, `minimum_phase_prototype`,
  `FirEq::generate_linear_phase_ir` / `generate_minimum_phase_ir`
- `FirEq` gains the cache as a field; resampler backends pass one through
- Interleaved paired benchmarks with an unchanged control, per the quality spec

**Out of scope**

- The `new()`-scoped planner sites, which already amortize correctly
- Test oracles and helpers, which stay independent
- Migrating the minimum-phase chain to `realfft` (measured 1.30-1.99x on the
  transform portion, tracked separately; transforms are only 28-38% of this
  chain's cost, so planner reuse comes first)
- `Complex::exp()` cost (23-40% of the chain; separate question)

## Completion criteria

1. No `*Planner::new()` remains on a repeatedly-called path in `src/` outside
   tests; each remaining one is `new()`-scoped or test-only.
2. `cargo test --all-features` and
   `cargo test --no-default-features --features rubato` pass.
3. `tests/public_api.rs` shows **no** surface change, `FirEq`'s `Send`/`Sync`
   included, verified by content rather than by assumption.
4. Existing minimum-phase and FIR EQ correctness tests pass **unchanged**, with
   no loosened tolerances, since output must stay bit-identical.
5. A test pins that repeated `regenerate_ir` calls produce byte-identical IRs to
   a freshly constructed `FirEq`, so cache reuse cannot silently drift.
6. `assert_no_alloc` steady-state tests still pass; nothing here may add
   allocation to a processing callback.
7. Interleaved before/after benchmarks recorded with an unchanged in-run control,
   covering both resampler setup and `FirEq::set_band`.
8. `cargo clippy --all-features --all-targets` and `cargo fmt --check` clean.
9. `docs/quality.md` updated from a fresh measured run, noting that the rubato
   path must be measured without default features.
