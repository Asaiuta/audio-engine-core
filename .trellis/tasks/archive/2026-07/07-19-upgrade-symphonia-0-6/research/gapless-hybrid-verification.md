# Hybrid Gapless Architecture Verification

Checked on 2026-07-19 after implementing codec-aware gapless ownership:

- Rust `1.93.1`, target `x86_64-pc-windows-msvc`
- CPU `Intel64 Family 6 Model 154 Stepping 3, GenuineIntel`
- Symphonia `0.6.0`, crate features `all`
- Project features `http,loudness-db`
- Three untimed warmups and nine ABBA-paired timed trials per path
- Command:

  ```text
  cargo bench --bench audio_gapless_comparison_perf -- \
    --enforce \
    --out .trellis/tasks/07-19-upgrade-symphonia-0-6/research/gapless-hybrid-verification-full.json
  ```

## Owner Policy

| Codec path | Gapless owner | Reason |
|---|---|---|
| MP3, Vorbis | Symphonia native decoder | These 0.6 decoders consume packet trim; Vorbis also discards reset preroll |
| AAC, ALAC, FLAC, PCM, other bundled codecs | Project Track fallback | Their 0.6 decoders do not consume `AudioDecoderOptions::gapless` |

The allowlist is intentionally codec-based and private. An upstream decoder is
not added until source inspection and a real fixture prove its native behavior.

## Correctness

| Fixture | Sequential output | Coarse seek |
|---|---|---|
| Stereo FLAC, 80 s | Exact frame/hash/sample match | Exact match |
| Stereo Ogg/Vorbis, 80 s | Exact frame/hash/sample match | Exact match |
| 6-channel FLAC, 20 s | Exact frame/hash/sample match | Exact match |
| 6-channel FLAC, 80 s | Exact frame/hash/sample match | Exact match |

All four validations report `pass`, and `--enforce` exits successfully. The
previous Ogg/Vorbis project-to-reference seek RMS of `0.0108423335` is now `0`;
the project/native seek chunk maximum and RMS deltas are also `0`.

## Decode Timing

Negative paired percentages mean the direct native reference was faster. These
numbers include wrapper/path overhead and were measured under ordinary desktop
load, so they are evidence of no large systematic regression, not portable
absolute throughput claims.

| Fixture | Project hybrid median | Native median | Paired native vs project |
|---|---:|---:|---:|
| Stereo FLAC, 80 s | 43.541 ms | 42.901 ms | +0.86% |
| Stereo Ogg/Vorbis, 80 s | 63.546 ms | 60.884 ms | -6.11% |
| 6-channel FLAC, 20 s | 46.762 ms | 49.618 ms | +6.80% |
| 6-channel FLAC, 80 s | 203.878 ms | 198.258 ms | -0.76% |

There is no consistent performance winner. Correctness and single-owner
semantics, not a noisy timing difference, determine the architecture.

## Remaining Coverage

- No real MP3/LAME fixture was available, so MP3 ownership is source-verified
  and unit-tested but not yet corpus-verified.
- No CAF fixture was available. CAF remains on Track fallback and is explicitly
  `skipped` in the JSON report.
- The report does not measure callback or end-to-end playback latency.
