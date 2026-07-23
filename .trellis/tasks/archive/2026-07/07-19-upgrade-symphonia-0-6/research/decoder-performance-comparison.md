# Symphonia 0.5.5 vs 0.6.0 Decoder Comparison

Checked on 2026-07-19 with Rust 1.93.1, `x86_64-pc-windows-msvc`, release
profile, Intel family 6 model 154. The comparator linked both versions into
one process from separate path packages and alternated the versions in ABBA
order for each input. The old detached worktree was temporarily renamed to
`audio-engine-core-baseline` only to avoid Cargo's same-name path-package lock
collision; decoder source was unchanged.

## Conditions

- Timed API: `StreamingDecoder::open` followed by borrowed
  `decode_next_borrowed()` until EOF. `open/probe/build` time and decode time
  were recorded separately; the primary metric is streaming decode time.
- Two untimed warmups per version and 31 timed trials per version per input.
- Full `decode_all()` output validation ran before timing. It checked sample
  rate, channel count, frame count, finite values, FNV output hashes, and a
  pointwise maximum/RMS delta.
- Both versions used the actual crate dependency configuration with
  `symphonia = { features = ["all"] }`. Symphonia 0.5.5 resolved as
  `all + default` without `opt-simd`; 0.6.0 resolved as `all + default +
  opt-simd` (SSE/AVX/NEON feature families). The crate's `http` and
  `loudness-db` features were disabled because the workload is local decode.
- The OS file cache was warm after validation and warmups. No network,
  resampling, DSP, or audio-device write was included.

## Corpus

| Input | Format | Duration | Channels | Frames | SHA-256 |
| --- | --- | ---: | ---: | ---: | --- |
| `seq-3341-3-16bit-v02.wav` | PCM WAV | 80 s | 2 | 3,840,000 | `1fced6ac2397d4337257185908bd49bb18314f441a0daf0ef602f671254d9efb` |
| `stereo_s16_48k_80s.flac` | FLAC | 80 s | 2 | 3,840,000 | `d99342b77a1882466966a2b8861ac47d6f5562b45d8f13532d908b29c001de8a` |
| `stereo_s16_48k_80s.ogg` | Ogg/Vorbis | 80 s | 2 | 3,840,000 | `962065605ba76643dccd31508ba391408e4a6fc3ce50c062f2b2c8fbd7722a54` |
| `surround_s16_48k_20s.flac` | 6-channel FLAC | 20 s | 6 | 960,000 | `179ccd3af89c900bb700769ac63350b2112994278b3cbd0705677673f7163442` |
| `surround_s16_48k_80s.flac` | 6-channel FLAC | 80 s | 6 | 3,840,000 | `d99a868511c711bae00d9b3a10cde8c08b9b3119145ac573e469973228873bd9` |

The first WAV is the checked-in EBU reference file. The FLAC/Vorbis files are
deterministic SoX 14.4.2 derivatives stored under the ignored `target/`
directory; the 80-second surround file repeats the 20-second reference four
times to reduce short-workload timing noise.

## Results

Each cell shows the old/new decode median in milliseconds for the two reversed
input-order runs, followed by the candidate change and speedup for each run.
Negative change means the 0.6 candidate is faster.

| Workload | Run 1 ms (0.5.5 -> 0.6.0) | Run 2 ms (0.5.5 -> 0.6.0) | Change | Speedup |
| --- | ---: | ---: | ---: | ---: |
| PCM stereo WAV, 80 s | 20.646 -> 15.816 | 34.981 -> 22.751 | -23.39% / -34.96% | 1.305x / 1.538x |
| Stereo FLAC, 80 s | 30.388 -> 26.676 | 32.703 -> 29.953 | -12.21% / -8.41% | 1.139x / 1.092x |
| Stereo Ogg/Vorbis, 80 s | 66.795 -> 50.543 | 57.669 -> 41.295 | -24.33% / -28.39% | 1.322x / 1.397x |
| 6-channel FLAC, 20 s | 52.127 -> 47.689 | 34.842 -> 30.765 | -8.51% / -11.70% | 1.093x / 1.133x |
| 6-channel FLAC, 80 s | 169.501 -> 161.172 | 164.172 -> 148.177 | -4.91% / -9.74% | 1.052x / 1.108x |

The two-run midpoint of the median changes is approximately -30.7% (WAV),
-10.2% (stereo FLAC), -26.2% (Vorbis), -9.8% (20-second surround FLAC), and
-7.3% (80-second surround FLAC). The open/probe portion was sub-millisecond
for lossless inputs and up to about 1.8 ms for Vorbis; it was not consistently
faster in 0.6, but it is a small fraction of total decode time.

## Output Compatibility

WAV and both FLAC workloads were bit-identical across versions, including
frame counts and hashes. Vorbis frame counts and metadata matched; decoded
floating-point samples differed at the expected tiny codec-rounding level
(maximum absolute delta `2.9802322387695312e-8`, RMS delta
`5.537594659693648e-9`). No non-finite samples or frame-count mismatches were
observed.

## Evidence Files And Limits

- Raw run 1: [`decoder-performance-comparison-final1.json`](decoder-performance-comparison-final1.json)
- Raw run 2: [`decoder-performance-comparison-final2.json`](decoder-performance-comparison-final2.json)
- Comparator source was a temporary external crate under `target/` so the two
  package versions could coexist; the report records its exact source paths,
  revisions, dirty state, and feature sets.
- This is a local streaming-decoder benchmark, not an end-to-end playback or
  callback benchmark. It does not include MP3/AAC, network sources, cold-disk
  latency, or the `decode_all` allocation path. The measured improvement is
  therefore strong evidence for this decode path and configuration, not a
  universal claim for every codec or player workload.
