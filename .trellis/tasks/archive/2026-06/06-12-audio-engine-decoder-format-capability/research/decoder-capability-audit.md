# Decoder Capability Audit (source-verified)

> Every claim below was checked against live source. A prior sub-agent summary
> contained at least one factual error (it claimed gapless trim runs only in
> `decode_all`); that error is corrected here. Trust this file, not the summary.

## Verified source anchors

- `Cargo.toml:19` — `symphonia = { version = "0.5", features = ["all"] }`. The
  full Symphonia codec/container set is compiled in (MP3, AAC/ALAC via isomp4,
  FLAC, Vorbis/OGG, WAV, AIFF, etc.). Format breadth is a build-feature fact,
  not separately gated in our code.
- `src/decoder/streaming.rs:118` — `decode_next_into(&mut Vec<f64>)` is the
  single decode primitive. Reuses `self.sample_buf` (a Symphonia
  `SampleBuffer`); only reallocates when packet capacity grows. Steady-state
  allocation-free into the caller's `out` Vec (caller owns growth).
- `src/decoder/streaming.rs:187-211` — **gapless trim lives in the streaming
  primitive**, not only in `decode_all`. Encoder delay is skipped at stream
  start (187-196); end padding is trimmed against `effective_total` (198-211).
  `decode_all` (228) just loops `decode_next_into`, so both paths trim
  identically. (This refutes the earlier audit's claim #3.)
- `src/decoder/streaming.rs:283-301` — `seek()` uses `SeekMode::Coarse` ONLY.
  No accurate/sample-exact mode is exposed. On seek it resets the decoder and
  zeroes `samples_output` (so delay re-trim re-applies from frame 0 after a
  seek — worth a test, may be wrong for mid-stream seeks).
- `src/decoder/error.rs:128-152` — `DecoderError` variants: `FileOpen`,
  `Network` (http only), `UnsupportedFormat`, `NoAudioTrack`, `Decoder(String)`,
  `Probe(String)`, `Canceled`.
- `src/decoder/metadata.rs` — rich tag extraction: title/artist/album/track/
  disc/genre/year/cover art/lyrics + ReplayGain (track/album gain/peak). Handles
  standard + non-standard tag keys. `AudioInfo` exposes sample_rate, channels
  (count only), bits_per_sample, total_frames, duration_secs, encoder_delay,
  end_padding.

## Real gaps (evidence-backed)

1. **Seek is Coarse-only and untested.** No `SeekMode::Accurate` path; no test
   asserts post-seek frame position within a tolerance. `seek()` maps all
   Symphonia errors to `Decoder(String)`, losing typed seek-failure info.
2. **Typed errors are constructed but not test-pinned.** `UnsupportedFormat` /
   `NoAudioTrack` exist, but probe/decode failures mostly funnel into
   `Decoder(String)` / `Probe(String)`. No fixture proves an unsupported input
   yields the *typed* variant rather than a stringly-typed generic error.
3. **No format→behavior coverage matrix.** `decoder/tests.rs` exists but is thin
   relative to the compiled-in format surface. No per-format decode/seek/
   metadata assertions.
4. **Corrupt/truncated input policy undefined.** No test asserts behavior on
   malformed streams. Note: `decode()` `DecodeError` packets are silently
   skipped (`streaming.rs:160`), which is a real (untested) policy.
5. **Channel layout positions not surfaced.** Only `channels: usize` (count).
   No `Channels` bitmask/positions. (Mixing is the sibling task's job; this task
   should expose layout metadata, not act on it.)
6. **Seek delay-trim correctness unverified.** `seek()` zeroes `samples_output`,
   so encoder-delay skip logic re-runs as if at frame 0 after every seek. For a
   non-zero seek target this likely mis-trims. Needs a test to confirm/deny.

## Correction log

- Earlier sub-agent summary claim "gapless trimming only applied in decode_all,
  NOT in streaming decode_next_into" is FALSE. Trim is in `decode_next_into`
  (187-211); `decode_all` only loops it.
