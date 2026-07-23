# Pre-change: Symphonia Native Gapless vs Project Manual Gapless

Checked on 2026-07-19 with the repository's optimized Cargo bench profile:

- Rust `1.93.1`, target `x86_64-pc-windows-msvc`
- CPU `Intel64 Family 6 Model 154 Stepping 3, GenuineIntel`
- Symphonia `0.6.0`, crate features `all`
- Project features `http,loudness-db`
- Working tree was dirty because the Symphonia upgrade and this benchmark were
  being developed in the same task
- Command:

  ```text
  cargo bench --bench audio_gapless_comparison_perf -- \
    --out .trellis/tasks/07-19-upgrade-symphonia-0-6/research/gapless-owner-comparison-full.json
  ```

The benchmark performs three untimed warmups and nine timed trials per mode in
deterministic ABBA order (the first path is reversed on every subsequent
round). It measures open/probe separately from borrowed streaming decode and
records each paired trial in the JSON report. Full output validation runs
before timing and checks frame count, finite samples, FNV-1a hash, maximum
absolute delta, RMS delta, and one coarse-seek probe. The process used inherited
priority; background desktop load was not isolated, so timing is a same-run
relative comparison rather than a portable absolute throughput claim.

## Paths Compared Before Architecture Update

| Path | Implementation |
|---|---|
| `project_manual` | Pre-change `StreamingDecoder`, Symphonia `gapless(false)`, crate-owned Track delay/padding trim |
| `native_gapless` | Direct Symphonia decoder with `AudioDecoderOptions::gapless(true)` |

## Correctness

| Fixture | Sequential frames (manual/native) | Sequential samples | Seek result |
|---|---:|---|---|
| Stereo FLAC, 80 s | 3,840,000 / 3,840,000 | FNV hashes identical; max/RMS delta `0` | Equivalent; seek RMS `0` |
| Stereo Ogg/Vorbis, 80 s | 3,840,000 / 3,840,000 | FNV hashes identical; max/RMS delta `0` | **Mismatch**; seek max delta `2.7791e-2`, RMS `1.6672e-2` |
| 6-channel FLAC, 20 s | 960,000 / 960,000 | FNV hashes identical; max/RMS delta `0` | Equivalent; seek RMS `0` |
| 6-channel FLAC, 80 s | 3,840,000 / 3,840,000 | FNV hashes identical; max/RMS delta `0` | Equivalent; seek RMS `0` |

The Ogg mismatch is reproducible across the quick and full runs. Both paths
landed on the same reported coarse frame, but the first post-seek chunk from
the manual path does not match the native reference. This is consistent with
the codec-reset/overlap behavior identified in the Symphonia source: native
Vorbis gapless mode discards the reset packet, while the project path has
gapless disabled and only applies the stream-level Track counters.

The full machine-readable report, including every raw timing sample and hash,
is [`gapless-owner-comparison-full.json`](gapless-owner-comparison-full.json).

## Decode Timing

Values below are median milliseconds for the nine timed trials. Negative
`native vs manual` means native gapless was faster.

The table gives the per-mode medians and the paired native/manual ratio from
the same ABBA rounds. A negative percentage means native gapless was faster;
the range is the minimum-to-maximum paired percentage across nine trials.

| Fixture | Manual decode (ms) | Native decode (ms) | Paired native vs manual |
|---|---:|---:|---:|
| Stereo FLAC, 80 s | 32.329 | 33.174 | +0.62% (-8.03..+7.31%) |
| Stereo Ogg/Vorbis, 80 s | 52.630 | 52.192 | -0.44% (-7.23..+11.65%) |
| 6-channel FLAC, 20 s | 60.580 | 54.176 | **-10.23%** (-16.30..+12.66%) |
| 6-channel FLAC, 80 s | 205.264 | 216.147 | -0.17% (-6.90..+47.87%) |

There is no universal speed winner. In this particular run native gapless was
about 10% faster on the short six-channel FLAC case, while the other paired
medians were within roughly 1% of the manual path; the wide per-trial ranges
show why the small differences should not be generalized. The trim operation
itself is not isolated from codec decode cost, so these are decoder-path
measurements, not a claim about callback or end-to-end playback latency.

## Scope Limits

- No checked-in MP3/LAME or CAF fixture is present. The JSON report records both
  as explicit `skipped` entries; those formats can be passed explicitly with
  `AUDIO_GAPLESS_FIXTURES` and the same comparator.
- The benchmark is report-only by default. `--enforce` intentionally fails on
  the observed Ogg seek mismatch in this pre-change report. The benchmark now
  verifies the implemented hybrid policy separately.
- The result does not prove that `gapless(true)` is a universal replacement:
  Symphonia's option is consumed codec-specifically, while the project Track
  fallback can cover metadata-bearing formats whose decoder does not consume
  packet trim fields.

## Decision Before Follow-up

For sequential decoding, both implementations are equivalent on the measured
corpus. For stateful codec seek behavior, native Symphonia gapless is more
correct on Ogg/Vorbis. Keep the project path as an explicit fallback for
formats without native trim consumption, and do not enable both owners for the
same format until real MP3 and CAF fixtures are added.

## Implemented Follow-up

The decoder now selects native gapless for MP3 and Vorbis, the only Symphonia
0.6 decoders that consume `AudioDecoderOptions::gapless`, and uses Track-level
fallback for all other codecs. The enforced post-change run makes the Ogg seek
chunk bit-identical to the native reference; see
[`gapless-hybrid-verification.md`](gapless-hybrid-verification.md).
