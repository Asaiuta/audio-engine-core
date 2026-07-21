# Research: rubato 4.0.0 as upgrade target from 0.16.2

- **Query**: Evaluate rubato 4.0.0 (HEnquist) vs our pinned 0.16.2 for `src/processor/resampler/rubato_backend.rs` (sinc backend + planned `FftFixedIn` path)
- **Scope**: external (crates.io API, GitHub repo/releases/PRs/issues, crate source 4.0.0 + 0.16.2) + empirical (compiled probe against real rubato 4.0.0, rustc 1.93.1, Windows, release build)
- **Date**: 2026-07-21

## Facts

### Version history and maturity

Source: crates.io API `https://crates.io/api/v1/crates/rubato` (fetched 2026-07-21).

| Version | Released | MSRV | License | Downloads |
|---|---|---|---|---|
| 0.16.2 | 2025-03-31 | 1.61 | MIT | 2,382,040 |
| 1.0.0-preview.0 / .1 | 2025-04-17 / 2025-10-24 | 1.71 | MIT | ~2,700 |
| 1.0.0 / 1.0.1 | 2025-12-30 / 2026-01-23 | 1.74 | MIT | 12,415 / 1,071,658 |
| 2.0.0 | 2026-04-01 | 1.85 | MIT | 120,095 |
| 3.0.0 | 2026-05-20 | 1.85 | MIT | 75,635 |
| **4.0.0** | **2026-07-09** | **1.85** | **MIT OR Apache-2.0** | 22,878 |

- The version numbering did not "jump" 0.16→4.0 in one step: 1.0 was a year-long API rework (preview.0 Apr 2025 → 1.0.0 Dec 2025), then three majors in 3.5 months (2.0, 3.0, 4.0), each driven largely by `audioadapter` major bumps (2.0 = audioadapter 3.0, 4.0 = audioadapter 4.0; release notes below).
- 4.0.0 is the latest; **no 4.0.x patch releases exist** (checked 2026-07-21). It is **12 days old**.
- What changed per major (sources: GitHub releases `https://github.com/HEnquist/rubato/releases`, tags v1.0.0/v2.0.0/v3.0.0/v4.0.0; README changelog in the 4.0.0 tarball):
  - **1.0.0**: new API via AudioAdapter crate; merged FixedIn/FixedOut/FixedInOut resamplers into single types (`Async`, `Fft`) with mode enums; merged sinc+polynomial async resamplers into one `Async` type.
  - **1.0.1**: fix `process_all_needed_output_len` calc; re-export audioadapter.
  - **2.0.0**: update to audioadapter 3.0, dependency updates.
  - **3.0.0** (perf release): "Improve sinc resampler performance with smarter dot product calculation. Improve SIMD performance (AVX, SSE, NEON) using multiple accumulators. Switch dot product strategy based on channel count. More aggressive inlining of hot paths." (README v3.0.0 changelog; PRs #130/#131 — no published benchmark numbers in the PRs or release notes.)
  - **4.0.0**: audioadapter 4.0 (Adapter/AdapterMut lifetime removed); fluent constructors + `Default` for `Indexing`/`SincInterpolationParameters`; `f_cutoff` now `Option<f32>` (automatic by default); `Fft::new` simplified + `new_custom` gains window selection; `process_all` one-shot API; `Resampler` split with capability traits `Adjustable`/`Resizable` (PR #132); new `Slip` clock-drift resampler (PR #134). Source: `https://github.com/HEnquist/rubato/pull/133` body + release v4.0.0.

### Constraints (question 5)

- **MSRV**: 1.85 (`rust-version = "1.85"` in the 4.0.0 tarball Cargo.toml; README: "requires rustc version 1.85 or newer"). Our crate MSRV 1.87 ⇒ **compatible**.
- **License**: changed MIT → **MIT OR Apache-2.0** at 4.0.0 (crates.io metadata; LICENSE-MIT + LICENSE-APACHE shipped in tarball). Still permissive.
- **Feature flags** (crates.io `/crates/rubato/4.0.0`): `default = ["fft_resampler"]`; `fft_resampler = ["realfft", "num-complex"]`; `log`; `bench_asyncro`. **`fft_resampler` is still the default** — same as 0.16.
- **New mandatory dependencies** vs 0.16.2 (tarball Cargo.tomls): `audioadapter = "4.0"` + `audioadapter-buffers = "4.0"` (both MIT OR Apache-2.0, MSRV 1.85, ~1.3M downloads), `windowfunctions 0.1.1` (MIT), `visibility 0.1.1` (Zlib OR MIT OR Apache-2.0), `num-integer`, `num-traits`. `realfft` bumped 3.3.0 → 3.5.0 (pulls rustfft 6.4.1; we already ship rustfft 6.2-series in-tree — no native deps anywhere). All permissive.
- **no_std**: rubato 4.0 is **std-only** (no `no_std` markers in lib.rs; uses `std::sync::Arc` etc.) — unchanged from 0.16.
- **f64**: fully supported (`Resampler<T: Sample>`, T = f32/f64; probe below ran `Async::<f64>` and `Fft::<f64>`).
- **Allocation-free processing**: README §"Real-time considerations": "Rubato is suitable for real-time applications when using the `Resampler::process_into_buffer()` method. This stores the output in a pre-allocated output buffer, and performs no allocations or other operations that may block the thread." **Empirically confirmed** — our probe wrapped every `process_into_buffer` call (sinc + FFT, with `None` and with `Indexing` incl. `partial_len`) in `assert_no_alloc` with the checking allocator active in release: zero aborts.

### API surface in 4.0.0 (question 2) — verified against tarball source

- `SincFixedIn` / `SincFixedOut` / `FastFixedIn/Out` **no longer exist** → single `Async<T>` + `enum FixedAsync { Input, Output }` (src/asynchro.rs:21, 99).
- `FftFixedIn` / `FftFixedOut` / `FftFixedInOut` **no longer exist** → single `Fft<T>` + `enum FixedSync { Input, Output, Both }` (src/synchro.rs:38, 52).
- Still exported with same names: `Resampler` trait, `SincInterpolationParameters`, `SincInterpolationType`, `WindowFunction` (incl. `BlackmanHarris2`), `calculate_cutoff::<T>(npoints, window)` (src/lib.rs:56-68, src/windows.rs:88).
- `SincInterpolationParameters` (src/asynchro_sinc.rs:26): `f_cutoff` is now `Option<f32>` (`None` = automatic via `calculate_cutoff` — exactly what we compute manually today). Builder: `SincInterpolationParameters::new(sinc_len, window).oversampling_factor(n).interpolation(t)`. Doc: `sinc_len` "will be rounded up to the nearest multiple of 8" (our 64/128/256 are already multiples of 8).
- `Async::<f64>::new_sinc(resample_ratio, max_resample_ratio_relative, &params, chunk_size, nbr_channels, fixed)` (src/asynchro.rs:247) — params now **by reference**; extra `FixedAsync` arg; `max_resample_ratio_relative = 1.0` accepted (probe).
- `Fft::<f64>::new(rate_in, rate_out, chunk_size, nbr_channels, fixed)` **auto-selects** `sub_chunks = (chunk_size / 256).max(1)` (src/synchro.rs:212-230); `Fft::new_custom(rate_in, rate_out, chunk_size, sub_chunks, nbr_channels, window, fixed)` (src/synchro.rs:278) exposes the 0.16-style `sub_chunks` knob **plus** the anti-aliasing window (0.16 hardcoded `BlackmanHarris2`, 0.16.2 src/synchro.rs:96-100 — pass it explicitly for identical behavior).
- `Resampler` trait (src/lib.rs:188): `process_into_buffer(&mut self, buffer_in: &dyn Adapter<T>, buffer_out: &mut dyn AdapterMut<T>, indexing: Option<&Indexing>) -> ResampleResult<(usize, usize)>` — same `(input_used, output_written)` return. `output_frames_max()`, `output_delay()`, `input_frames_next()`, `output_frames_next()`, `reset()` all keep their names/semantics. New: `Indexing { input_offset, output_offset, partial_len, active_channels_mask }` (src/lib.rs:76) — `partial_len: Some(n)` feeds a short final chunk padded with silence; `Some(0)` processes pure silence "to flush the resampler delay". New capability traits `Adjustable`/`Resizable` only matter to trait implementors, not consumers.
- Buffers use the `audioadapter` crate (re-exported as `rubato::audioadapter` / `rubato::audioadapter_buffers`). For mono slices, `audioadapter_buffers::direct::InterleavedSlice::new(&slice, 1, frames)` / `new_mut` wrap `&[f64]` / `&mut [f64]` directly (used in probe; non-allocating wrapper). README recommends `SequentialSliceOfVecs` for 0.16-style `Vec<Vec<T>>`.
- README ships a "Migrating from 3.x to 4.0" guide (README.md:410).

### Delay/priming semantics (question 3) — source + empirical

**This is the one real behavior change for our adapter.**

- 0.16.2 sinc: interpolation index initialized at `-(sinc_len / 2)` (0.16.2 src/asynchro_sinc.rs:554) → output positionally pre-aligned; our backend deliberately does **not** skip `output_delay()` (rubato_backend.rs:10-14, validated by our impulse tests).
- 4.0.0 sinc: `InnerSinc::init_last_index = -(nbr_points - 1)` (src/asynchro_sinc.rs:601-603) → **output now carries a real leading delay ≈ `output_delay()`**; `output_delay = (nbr_points * ratio / 2) as usize` (src/asynchro.rs:574-576, same formula as 0.16). The 4.0 example and `process_all_into_buffer` both trim `output_delay()` leading frames ("initial silence (caused by the resampler delay) trimmed off", src/lib.rs).
- 4.0.0 Fft: `output_delay() = fft_size_out / 2` (src/synchro.rs:656-658) — **identical formula to 0.16.2 FftFixedIn** (0.16.2 src/synchro.rs:656-658). Same rational-ratio design: sizes from `gcd(rate_in, rate_out)`; sub-chunk rounded up to multiples of `rate / gcd` (44100→48000 ⇒ min blocks 147/160 — same 147/160 math as 0.16).

**Empirical probe** (our own build vs real rubato 4.0.0, mono f64, chunk 1024, 44100→48000, temp project outside repo):

```
[Async sinc len256/BH2/os256/Cubic] input_frames_next=1024  output_frames_max=1124  output_delay=139
    impulse fed at input frame 0 -> argmax at OUTPUT frame 138 (peak 0.83)   <- real delay now, was ~0 in 0.16
[Fft new_custom(44100,48000,1024,sub_chunks=2,1,BH2,FixedSync::Input)] output_frames_max=1280 output_delay=320
    impulse -> argmax at OUTPUT frame 320 exactly (peak 0.98); per-call outputs [640,1280,1280,640] (avg 960 = 1024*160/147)
[Fft::new auto sub_chunks=4] output_delay=160; impulse -> frame 160 exactly
[Fft partial_len=100] (used,written)=(1024,640) no alloc;  [partial_len=0 flush] (1024,1280) no alloc
[Fft 44100->44101 pathological, gcd=1] constructs OK in ~12 ms; input_next=1024 BUT output_frames_max=44101, output_delay=22050
```

So: a leading-delay-skip adapter works for **both** paths in 4.0 with the same mechanism, and skipping exactly `output_delay()` frames lands the sinc impulse within 1 frame (138 vs 139 — inside our ±1 impulse-alignment test tolerance; FFT is exact). The pathological-ratio result confirms the PRD's sinc-fallback plan is still required in 4.0 (constructible but 0.5 s delay + 44101-frame output chunks).

### Performance and quality (question 4)

- Sinc path: v3.0.0 release claims (carried into 4.0): smarter dot products, multiple-accumulator AVX/SSE/NEON, per-channel-count strategy, hot-path inlining (README changelog; PRs #130/#131 merged 2026-05-20). **No published benchmark numbers** found in releases, PRs, or PR comments. New doc table (src/asynchro_sinc.rs) puts mono Cubic at 4 dot-products/output frame — same algorithmic cost class as 0.16; the gains are implementation-level, magnitude unverified.
- FFT path: algorithm unchanged (same overlap-based rational-ratio FFT design, realfft 3.3→3.5); runtime SIMD selection for sinc unchanged (x86_64 AVX→SSE3 fallback, aarch64 NEON; README §SIMD acceleration). Our PRD's measured 0.16.2 `FftFixedIn` numbers (8.8 ns/sample 44.1k→48k) come from the same design that 4.0's `Fft` implements.
- Open issue #135 "Polyphase Resampler Optimization for Rubato" (2026-07-17) and open PR #138 (Horner-form poly eval, 2026-07-18) show active perf work, none of it released.

### Risk signals (question 6)

- **Open issues against 4.0** (only 2 open issues total, both 2026-07-18): **#136 — panic (index OOB / unsafe precondition) in `asynchro` resamplers during dynamic ratio transitions with `ramp = true`**; fix PR #137 still open, would need a 4.0.1. *Does not touch our usage*: we never call `set_resample_ratio` (fixed ratio, `max_resample_ratio_relative = 1.0`). #135 is a feature proposal. Sources: `https://github.com/HEnquist/rubato/issues/136`, `/135`.
- **0.16.x maintenance**: last 0.16 release 2025-03-31; **no 0.16 maintenance branch exists** (branch list = master + feature branches; development moved to 1.0-preview in Apr 2025). 0.16.2 is effectively frozen — bugfixes land only on the 4.x line.
- **Adoption** (crates.io reverse deps, 185 dependents, top-100 page by recent downloads): majority still on 0.12–0.16 (songbird, web-audio-api, rusty-chromaprint, mistralrs-core, deep_filter, hrtf...). Post-1.0 API adopters: `fixed-resample ^4.0` (55k recent dl), `arie ^4.0.0`, `bliss-audio ^3.0.0`, `mtrack ^3`, `audio_tools ^3.0`, `candle-examples ^2`, `flac-codec ^2.0`, `sofar ^1.0.1`, ... (4.x: 3 crates; 3.x: 14; 2.x: 7; 1.x: 8 on that page). CamillaDSP (author's flagship) master pins `rubato = "1.0"`. Source: `https://crates.io/api/v1/crates/rubato/reverse_dependencies`, `https://raw.githubusercontent.com/HEnquist/camilladsp/master/Cargo.toml`.
- **Release cadence risk**: three majors in 3.5 months, each tracking an `audioadapter` major — a rubato 5.0 on audioadapter 5.0 is plausible on a months scale. Pinning `=4.0.0`-style (or `"4.0"`) insulates us; our adapter isolates rubato types behind ~240 lines.
- Repo health: 346 stars, pushed 2026-07-18, single maintainer (HEnquist) doing all merges.

## Our migration surface (0.16.2 → 4.0.0)

Backend adapter: `src/processor/resampler/rubato_backend.rs` (240 lines). All mappings compile-and-run verified by the probe.

| Our 0.16.2 call (line) | rubato 4.0.0 equivalent | Effort |
|---|---|---|
| `use rubato::{calculate_cutoff, Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction}` (l.28) | `use rubato::{Async, FixedAsync, Indexing, Resampler, SincInterpolationParameters, SincInterpolationType, WindowFunction}; use rubato::audioadapter_buffers::direct::InterleavedSlice;` (+ `Fft, FixedSync` for FFT path) | trivial |
| `SincInterpolationParameters { sinc_len, f_cutoff: calculate_cutoff::<f32>(sinc_len, BH2), oversampling_factor, interpolation, window }` (l.45-59) | `SincInterpolationParameters::new(sinc_len, WindowFunction::BlackmanHarris2).oversampling_factor(osf).interpolation(interp)` — `f_cutoff: None` auto-derives the identical `calculate_cutoff` value | ~8 lines |
| `SincFixedIn::<f64>::new(ratio, 1.0, params, 1024, 1)` (l.98) | `Async::<f64>::new_sinc(ratio, 1.0, &params, 1024, 1, FixedAsync::Input)` | 1 line |
| field `resampler: SincFixedIn<f64>` (l.69); `out_stage: Vec<Vec<f64>>` (l.73) | `Async<f64>` (or small enum over `Async`/`Fft` for FFT routing); `out_stage` flattens to `Vec<f64>` (mono) | ~10 lines |
| `process_into_buffer(&[&self.in_fifo[..CHUNK_IN]], &mut self.out_stage, None)` (l.125) | `process_into_buffer(&InterleavedSlice::new(&self.in_fifo[..CHUNK_IN], 1, CHUNK_IN)?, &mut InterleavedSlice::new_mut(&mut self.out_stage, 1, out_max)?, None)` — same `(used, written)` tuple; adapter wrappers are non-allocating stack structs | ~12 lines |
| `output_frames_max()` (l.100), `reset()` (l.231) | unchanged names/semantics | 0 |
| `output_delay()` — currently intentionally ignored for sinc (l.10-14) | **must now skip `output_delay()` leading frames** (sinc no longer pre-compensates); same skip logic the planned FFT path needs; drain's `expected_total` accounting must add the skipped frames (extra zero-flush rounds; `Indexing::partial_len(0)` can replace our manual `zero_chunk` padding) | ~25-35 lines, the only semantic change |
| planned `FftFixedIn::<f64>::new(from, to, 1024, 2, 1)` (0.16.2 signature synchro.rs:512) | `Fft::<f64>::new_custom(from, to, 1024, 2, 1, WindowFunction::BlackmanHarris2, FixedSync::Input)` — same sub_chunks/gcd semantics, same `fft_size_out/2` delay (=320 for 44.1k→48k), BH2 arg reproduces 0.16's hardcoded window | 1 line vs 0.16 plan |
| module doc + tests | rewrite l.10-14 delay contract; impulse/duration/no-alloc/chunking tests should pass unchanged **after** the delay skip (probe: sinc within ±1, FFT exact; no-alloc confirmed) | ~15 lines docs |

**Total estimate: ~70-90 lines touched of 240; no architectural change** (FIFO chunk adaptation, drain padding/truncation, bitwise chunking invariance all carry over). The delay-skip mechanism is shared work with the FFT integration this task already requires.

## Risks

1. **Freshness**: 4.0.0 is 12 days old, zero patch releases, 22.9k downloads; one correctness bug (#136 panic) already found in its first 10 days — though in ramped-ratio code we never execute.
2. **Sinc delay semantics flip**: if migrated without adding the skip, every sinc output shifts by ~`output_delay()` frames (139 @ High/44.1k→48k) — our impulse-alignment tests (±1 frame) will catch this deterministically.
3. **Fast major cadence**: audioadapter-driven majors (2.0/3.0/4.0 in 3.5 months) mean future upgrade churn; mitigated by version pinning and the thin adapter.
4. **Unverified perf claims**: 3.0's sinc speedups have no published numbers; do not assume the 16-27x sinc gap closes from the upgrade alone — the PRD's FFT-path strategy remains the performance plan.
5. **Pathological ratios**: 4.0 `Fft` constructs 44100→44101 fine but with 22050-frame delay and 44101-frame output chunks — the planned sinc fallback for non-smooth ratios stays necessary.
6. **Ecosystem lag**: most downstream crates still on 0.x, so community bug discovery on 4.0 is thin; counterweight: `fixed-resample` (realtime-focused) already tracks ^4.0 and CamillaDSP exercises the same post-1.0 architecture.

## Bottom line

**Upgrade now (as stage one of this task), pinning `rubato = "4.0"`.** Every hard constraint checked out against primary sources and a compiled probe: MSRV 1.85 ≤ our 1.87, MIT OR Apache-2.0 license, `fft_resampler` still default, f64 supported, and `process_into_buffer` verified allocation-free under an active `assert_no_alloc` allocator — while 0.16.2 has been frozen since 2025-03-31 with no maintenance branch, so staying means building this task's new FFT adapter code against a dead line and re-touching the sinc path (plus a second full 27-gate evidence run) at whatever later date the upgrade happens. The migration is bounded (~70-90 lines of a 240-line adapter, mappings verified compile-and-run) and its only behavioral change — sinc output now carrying a real `output_delay()` leading delay (measured 138 vs reported 139) — requires exactly the leading-delay-skip mechanism the `Fft` path (delay 320, measured exact) needs anyway, letting both paths share one delay contract and one evidence run. The counter-evidence is 4.0.0's freshness (12 days, no patch release, open panic bug #136), but that bug sits in ramped-ratio code our fixed-ratio backend never calls, and our deterministic quality gates (impulse ±1, bitwise chunking, no-alloc, 27 fidelity gates) cover precisely the regression classes a young release could carry.

## Sources

- crates.io: `https://crates.io/api/v1/crates/rubato`, `.../rubato/4.0.0`, `.../rubato/reverse_dependencies`, `.../audioadapter`, `.../audioadapter-buffers`, `.../windowfunctions`, `.../visibility`
- GitHub: `https://github.com/HEnquist/rubato` (releases v0.16.2-v4.0.0; PRs #130 #131 #132 #133 #134; issues #135 #136; branches; repo meta), `https://raw.githubusercontent.com/HEnquist/camilladsp/master/Cargo.toml`
- docs.rs: `https://docs.rs/rubato/4.0.0`
- Crate tarballs (extracted to system temp, not repo): `https://static.crates.io/crates/rubato/rubato-4.0.0.crate` and `rubato-0.16.2.crate` — cited files: 4.0.0 `src/lib.rs`, `src/asynchro.rs`, `src/asynchro_sinc.rs`, `src/synchro.rs`, `src/windows.rs`, `README.md`, `examples/process_f64.rs`, `Cargo.toml`; 0.16.2 `src/asynchro_sinc.rs`, `src/synchro.rs`, `Cargo.toml`
- Empirical probe: temp cargo project (`rubato = "=4.0.0"`, `assert_no_alloc` with default-features off, release build, rustc 1.93.1, Windows 11) — impulse-delay, no-alloc, `partial_len`, and pathological-ratio measurements reproduced verbatim above
