# Measurements

Host: Windows x86_64, `f64`, 48 kHz. This machine is noisy: repeated `--quick`
runs of the same unmodified binary vary by up to ~150% on the convolver
throughput cases. Absolute single-run numbers are therefore not quotable here.
Every claim below is a **paired A/B** — before and after runs interleaved on the
same host in the same session — with the medians of 3 runs per side.

## A1 loudness meter

`Mode::all()` -> `Mode::I | Mode::LRA | Mode::HISTOGRAM`, plus gating readers
moved out of `process`.

| Case | before | after | change |
| --- | ---: | ---: | ---: |
| `loudness_meter process` stereo 512-frame (ns/input-sample) | 247.60 | 20.59 | **-91.7%** |
| `loudness_meter process` stereo 4096-frame (ns/input-sample) | 62.09 | 20.49 | **-67.0%** |

Raw run triples: before 512 = [226.5, 247.6, 271.6], after 512 = [20.9, 20.6, 19.9];
before 4096 = [59.2, 66.7, 62.1], after 4096 = [20.0, 21.6, 20.5].

### Bit-exactness of the mode change

`probe-ebur128-mode.rs`, level-stepped signal so LRA is non-zero (LRA = 11.6):

| mode | I / S / M / LRA | ingest |
| --- | --- | ---: |
| `Mode::all()` | baseline | 32.57 ns/sample |
| `I \| LRA \| HISTOGRAM` | **bit-equal** | 14.25 ns/sample |
| `I \| LRA` | **differs**, I off by 0.002-0.012 LU | 13.42 ns/sample |

`HISTOGRAM` must stay. Pinned by
`narrowed_mode_matches_mode_all_bit_for_bit`.

## A2 convolver: complex rustfft -> realfft

`--quick --pinned`, `process_into` median ns/sample. 28 of 28 cases faster.

| case | before | after | change |
| --- | ---: | ---: | ---: |
| 256 taps, 2 ch | 8.9 | 5.5 | -38.6% |
| 256 taps, 6 ch | 10.0 | 5.6 | -43.3% |
| 2048 taps, 2 ch | 15.0 | 6.9 | -54.0% |
| 2048 taps, 6 ch | 14.2 | 7.5 | -47.5% |
| 4097 taps, 2 ch | 25.1 | 18.3 | -27.1% |
| 8192 taps, 2 ch | 31.4 | 21.4 | -31.7% |
| 16384 taps, 2 ch | 39.3 | 31.9 | -18.8% |
| 32768 taps, 2 ch | 58.0 | 52.1 | -10.1% |
| 65536 taps, 2 ch | 102.1 | 90.8 | -11.1% |
| 65536 taps, 6 ch | 173.2 | 130.9 | -24.4% |

FIR EQ (`audio_fir_eq_perf --quick`):

| case | change |
| --- | ---: |
| apply 511-tap stereo | -29% .. -51% |
| apply 1023-tap stereo | -37% .. -53% |
| apply 2047-tap stereo | -57% |
| regeneration 511-2047, linear + minimum phase | -6% .. -35% |

An unpinned run of the same build showed apparent *regressions* on the
partitioned cases; pinning removed them entirely. Do not trust unpinned
convolver deltas on this host.

## Tier B dependency removal

`cargo tree -e normal --no-dedupe`, unique crates:

| build | before | after |
| --- | ---: | ---: |
| default (`soxr,http,loudness-db`) | 151 | **142** |
| pure Rust (`rubato`) | 89 | **80** |

`rayon`, `arc-swap`, `atomic_float` all gone.

## Deferred: in-house R128 meter

`probe-inhouse-r128-deferred.rs` measured ~5x ingest vs `ebur128` with I within
0.05 LU and M within 0.004 LU. Not pursued: needs the EBU Tech 3341/3342
conformance corpus, which is currently skipped in this repo. Separate task.
