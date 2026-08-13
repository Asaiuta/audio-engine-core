# Reclaim measured performance from existing dependencies

## Goal

Remove measured waste that exists **because of how this crate calls its
dependencies**, not because of any missing algorithm. Three independent
workstreams, all with unchanged public API behavior:

- **A1** `LoudnessMeter` asks `ebur128` for work it never reads, and recomputes
  gating windows on every block.
- **A2** `OverlapSaveConvolver` runs a complex FFT over real-valued audio while
  `realfft` is already a dependency (the partitioned engine already uses it).
- **B** Three dependencies are each used in exactly one place and are smaller
  than the code already written next to them.

No new DSP algorithm is introduced. No public signature changes.

## What I already know (measured, this machine, 2026-08-13)

All figures below are from throwaway probes built against the same crate
versions in `Cargo.lock` (`ebur128 0.1.10`, `rustfft 6.4.1`, `realfft 3.5.0`),
release profile, f64, 48 kHz stereo. They are **direction-of-effect evidence**
for planning, not gate values — Phase 2 must reproduce them through the
crate's own benches.

### A1① `Mode::all()` buys TRUE_PEAK that the crate never reads

`meter.rs:228` constructs `ebur128::Mode::all()`. `Mode::all()` includes
`TRUE_PEAK` and `SAMPLE_PEAK`, but `ebur128.true_peak()` / `.sample_peak()`
have **zero call sites** in `src/` (verified by grep). The crate computes true
peak itself with its own 49-tap 4x polyphase FIR (`TruePeakDetector`), which is
the value `LoudnessMeter::true_peak()` actually returns. So intersample peak is
computed twice per sample and one of the two results is discarded.

Critically, `Mode::all()` **also** includes `HISTOGRAM`, which changes gating
results. The correct replacement is therefore `I | LRA | HISTOGRAM`, not
`I | LRA`:

| Mode | I / S / M / LRA vs `all()` | ingest |
| --- | --- | --- |
| `Mode::all()` (current) | baseline | 32.57 ns/sample |
| `I \| LRA \| HISTOGRAM` | **bit-equal** (varying-level signal, LRA = 11.6) | **14.25 ns/sample** |
| `I \| LRA` (no histogram) | **differs** (I off by 0.002–0.012 LU) | 13.42 ns/sample |

→ `I | LRA | HISTOGRAM` is bit-exact and **2.29x** faster. Dropping `HISTOGRAM`
is a correctness change and is **out of scope**.

### A1② `process()` recomputes gating windows on every block

`meter.rs:149-163` calls all four getters (`loudness_global`,
`loudness_shortterm`, `loudness_momentary`, `loudness_range`) on every
`process()` and caches into fields. In `ebur128`, `loudness_momentary` and
`loudness_shortterm` go through `energy_in_interval` →
`Filter::calc_gating_block`, which re-scans the whole 400 ms / 3 s window each
call — cost independent of the block just ingested:

| block size | ingest | 4 getters | getter share |
| --- | --- | --- | --- |
| 512 | 29.3 ns/sample | 309.2 ns/sample | **91%** |
| 4096 | 30.1 ns/sample | 38.6 ns/sample | **56%** |
| 8192 | 30.1 ns/sample | 25.3 ns/sample | 46% |

`loudness_momentary` alone measured 18–24 µs per call regardless of block size.
The documented 42.37 ns/input-sample figure in `docs/quality.md` (4096-frame
loudness analysis) is majority getter cost, not ingest cost.

**User decision (2026-08-13):** internal lazy evaluation, public behavior
completely unchanged. `process()` marks dirty; getters evaluate on demand and
cache. Same signatures, same return values, no new public API.

### A2 Real-valued audio through a complex FFT

`PartitionedConvolver` (long IR) already uses `realfft`. `OverlapSaveConvolver`
— which serves short IRs **and** is the head path of the partitioned engine,
and which backs FIR EQ — uses `rustfft` complex transforms with an
identically-zero imaginary part (`convolver.rs:308-334`, `:399`, `:447`).

| N | complex fwd+inv | real fwd+inv | speedup |
| --- | --- | --- | --- |
| 512 | 1.80 µs | 1.05 µs | **1.71x** |
| 1024 | 3.47 µs | 2.34 µs | **1.48x** |
| 2048 | 8.13 µs | 6.71 µs | **1.21x** |

No new dependency; `realfft` is already in `Cargo.toml` and already used in this
exact file. Spectrum storage also halves (`fft_size` → `fft_size/2 + 1`).

Affected published baselines: `FFTConvolver` 9.39 ns/sample, FIR EQ apply
10.9 ns/sample (`docs/quality.md`).

### B Three single-use dependencies

| dep | actual usage | note |
| --- | --- | --- |
| `rayon` | **1 site**: `resampler/mod.rs:461` `into_par_iter()` inside `resample_parallel` | `resample_parallel` has **no caller** anywhere in `src/`, `benches/`, `examples/`, or `docs/` — only its own tests. Offline one-shot, ≤ 8 channels. |
| `atomic_float` | 7 `AtomicF64` fields across `lockfree_params.rs` + `loudness/atomic_state.rs` | `AtomicU64` + `to_bits`/`from_bits` is the same codegen. |
| `arc-swap` | **1 field**: `SharedParams.current` | Serves control-side reads only. The realtime path is already the crate's own epoch-reclaimed `RealtimeSnapshot`. |

Dependency counts (`cargo tree -e normal`, deduped):

| build | now | after B |
| --- | --- | --- |
| default (`soxr,http,loudness-db`) | 151 | 143 |
| pure Rust (`rubato`) | 89 | 81 |

`rayon` pulls `rayon-core`, `crossbeam-deque`, `crossbeam-epoch`,
`crossbeam-utils`, `either` (6 total).

## Decision (ADR-lite)

**Context.** All three workstreams remove waste caused by *how dependencies are
invoked*, not by missing capability. Each is independently revertable and each
has a same-machine measurement path through an existing bench.

**Decision.**

1. **A1① — narrow the mode to `I | LRA | HISTOGRAM`.** Bit-exactness against
   `Mode::all()` is a *hard gate*, proven by a test that runs a varying-level
   signal (so LRA is non-zero) through both modes and asserts bit equality on
   all four measurements. Do **not** drop `HISTOGRAM`.
2. **A1② — lazy getters, unchanged public behavior.** `process()` stops calling
   getters and sets a dirty flag; each public getter evaluates on demand and
   memoizes until the next `process()`/`reset()`. Signatures and returned values
   are unchanged, so this is not a SemVer event.
3. **A2 — `OverlapSaveConvolver` moves to `realfft`.** Mirrors what
   `PartitionedConvolver` already does in the same file. The existing
   direct-convolution oracle test is the correctness gate; FFT rounding
   tolerance follows the precedent in commit `df1346a`.
4. **B — replace all three single-use dependencies.**
   - `rayon` → `std::thread::scope`, keeping the `resample_parallel` signature
     and its error semantics byte-for-byte.
   - `atomic_float` → a private `AtomicF64(AtomicU64)` newtype.
   - `arc-swap` → keep the `Arc<T>` public surface; back it with the crate's
     existing generation/writer-lock machinery.

**Consequences / risks.**

- **`load_if_changed` pointer identity is the one real hazard.**
  `lockfree_params.rs:330` compares `std::ptr::eq(&**current, Arc::as_ref(cached))`.
  Any `arc-swap` replacement must preserve "same published snapshot ⇒ same
  allocation ⇒ `None`". A `Mutex<Arc<T>>` holding the identical `Arc` preserves
  this; rebuilding an `Arc` per read would silently break it into "always
  changed". This needs a dedicated test.
- `load_if_changed` / `load_if_changed_since` / `load` / `load_with_generation`
  are **public** (present in `tests/public-api-all-features.txt`), so the
  `public_api` baseline must stay unchanged — this is a pure internal swap.
- A1① changes what `ebur128` computes internally. If a future task ever wants
  `ebur128`'s own true peak, the mode must be widened again; a comment must
  record why `TRUE_PEAK` is intentionally absent.
- Bench baselines in `docs/quality.md` (42.37 ns loudness, 9.39 ns convolver,
  10.9 ns FIR EQ) will move. Per `quality-guidelines.md` the doc numbers may
  only be restated from a fresh measured run, never estimated.

## Scope

**In scope**

- `src/processor/loudness/meter.rs` — mode narrowing + lazy getters
- `src/processor/convolver.rs` — `OverlapSaveConvolver` → `realfft`
- `src/processor/resampler/mod.rs` — `rayon` → `std::thread::scope`
- `src/processor/lockfree_params.rs` — `arc-swap` + `atomic_float` removal
- `src/processor/loudness/atomic_state.rs` — `atomic_float` removal
- `Cargo.toml` — drop `rayon`, `atomic_float`, `arc-swap`
- Fresh bench runs + `docs/quality.md` updates for every number that moves

**Out of scope** (explicitly deferred)

- Replacing `ebur128` with an in-house R128 meter (prototype measured 4.98x
  ingest and bit-plausible agreement: I within 0.05 LU, M within 0.004 LU — but
  it needs the EBU Tech 3341/3342 conformance corpus, which is currently
  `skipped`). Separate task.
- Dropping `HISTOGRAM` from the mode — changes results.
- Removing `soxr` / `rubato` / `rustfft` / `symphonia` / `reqwest` / `rusqlite`.
- Replacing `reqwest` with a smaller HTTP client.
- Any public API addition or signature change.

## Completion criteria

1. `cargo test --all-features` and `cargo test --no-default-features --features rubato` pass.
2. New test: `Mode::all()` vs `I|LRA|HISTOGRAM` bit equality on I/S/M/LRA over a
   varying-level signal (LRA must be non-zero, else the test proves nothing).
3. New test: lazy getters return values identical to the current eager
   implementation across block boundaries, after `reset()`, and before the first
   reliable measurement.
4. New test: `load_if_changed` still returns `None` for an unchanged published
   snapshot and `Some` after exactly one publish.
5. Convolver direct-convolution oracle, reset/tail exactness, and chunking
   invariance tests still pass.
6. `assert_no_alloc` steady-state tests still pass for meter, convolver, and the
   DSP chain.
7. `tests/public_api.rs` baseline **unchanged** (byte-identical) for both
   feature sets.
8. `cargo tree -e normal` shows `rayon`, `atomic_float`, `arc-swap` gone;
   default build ≤ 143 crates.
9. Fresh `audio_component_perf`, `audio_convolver_perf`, `audio_fir_eq_perf`,
   `audio_callback_chain_perf` runs recorded under the task's `research/`, and
   every affected number in `docs/quality.md` restated from those runs.

---

## Outcome (2026-08-13)

All four workstreams landed. Numbers are paired A/B medians on one host; see
`research/measurements.md` for raw runs and the noise caveat.

| Item | Result |
| --- | --- |
| A1① mode narrowing | `I \| LRA \| HISTOGRAM`, bit-exact vs `Mode::all()` |
| A1② deferred gating reads | 512-frame block **-92%**, 4096-frame **-67%** per input sample |
| A2 realfft convolver | 28/28 pinned cases faster, **-10% .. -54%**; FIR EQ apply **-29% .. -57%**; spectral storage halved |
| B dependency removal | default 151 → **142**, pure-Rust 89 → **80** |

### Deviations from the plan

1. **A1② is not a cache.** The PRD assumed lazy-with-memoization behind `&self`.
   Any interior mutability (`Cell`) strips `LoudnessMeter` of `Sync` and
   `Freeze`, which *is* a breaking public change — caught by the `public_api`
   baseline. The readers now query the backend directly and hold no cache, which
   delivers the same win because the cost was the eager per-block evaluation,
   not repeated reads. Documented in `gating_measurement`.

2. **`HISTOGRAM` is load-bearing.** The original analysis proposed `I | LRA`.
   That changes integrated loudness by 0.002–0.012 LU. Corrected before any code
   was written, and pinned by a test whose fixture steps level so LRA ≠ 0 (a
   constant-level fixture reports LRA = 0 under every mode and proves nothing).

3. **Public API surface widened, not preserved.** Completion criterion 7 wanted a
   byte-identical baseline. 28 auto-trait lines changed across the two
   baselines — all `!UnwindSafe`/`!RefUnwindSafe` → `UnwindSafe`/`RefUnwindSafe`
   on 8 types, because `atomic_float`'s `UnsafeCell<f64>` representation was
   replaced by `AtomicU64`. Gaining an auto trait is non-breaking, so the
   baselines were regenerated; verified 0 non-unwind changes.

4. **`resample_parallel` gained its first real tests.** It previously had only
   error-path coverage. Added bit-exact per-channel equivalence across 1/2/6/8
   channels and a barrier-based concurrency assertion, so a silent regression to
   a sequential loop now fails.

### Pre-existing issue found (not fixed here)

`tests/public_api.rs` fails on a pristine tree on Windows: the committed
baselines are CRLF, `public-api` renders LF, so the string compare always
mismatches locally. CI checks out LF and passes. Verified by stashing all
changes and re-running. Out of scope for this task; worth its own fix
(`.gitattributes` with `tests/public-api-*.txt -text`, or normalize in the test).
