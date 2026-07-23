# Symphonia 0.6 Migration Research

## Sources

- Symphonia v0.6.0 release: <https://github.com/pdeljanov/Symphonia/releases/tag/v0.6.0>
- Official 0.5-to-0.6 migration guide: <https://github.com/pdeljanov/Symphonia/blob/v0.6.0/docs/guides/migration/0p6.md>
- crates.io sparse index entry, checked 2026-07-19: `symphonia 0.6.0` is the latest non-yanked stable release and requires Rust 1.85. The crate declares Rust 1.87 because existing DSP code uses `is_multiple_of`, stabilized in 1.87.
- Current repository dependency: `Cargo.toml` requests `symphonia = { version = "0.5", features = ["all"] }`; `Cargo.lock` resolves 0.5.5.

## Upgrade Value

- SIMD is enabled by default in 0.6 through `opt-simd`; the current 0.5.5 `all` feature does not enable SIMD.
- The release reports decoding performance improvements and reduced binary size, but publishes no workload-specific numbers.
- Probe scoring is intended to reduce false-positive format detection.
- Matroska and MP4 demuxers were made safer, AIFF/CAF support improved, and fuzzing-discovered panic paths were fixed.
- Codec and track parameters are populated more reliably.
- Metadata gains include ID3v1, APEv1/APEv2, Matroska metadata, chapters, and typed standard tags.
- The audio codec set is materially unchanged: AAC, ADPCM, ALAC, FLAC, MPEG layers I/II/III, PCM, and Vorbis. This upgrade does not add Opus.
- Zero-copy decoding of non-owning packets mainly benefits applications using a non-Symphonia demuxer; this crate uses Symphonia format readers, so that item is not an expected direct win.

## Breaking Changes That Affect This Crate

### `src/decoder/streaming.rs`

- `SignalSpec` becomes `AudioSpec`.
- `SampleBuffer` is removed; decoded audio must be copied/interleaved through the new audio traits or retained via the new generic buffer reference API.
- `Decoder`/`DecoderOptions` become audio-specific types, and registry construction moves from `make` to `make_audio_decoder`.
- `FormatReader::next_packet()` returns `Result<Option<Packet>>`; EOF is no longer represented by an I/O error.
- Probe moved under `core::formats`; `format(...)` becomes `probe(...)` and returns the format reader directly.
- Track codec parameters are optional, while timing, duration, delay, and padding move from codec parameters to the track.
- Time, duration, timestamp, and seek-related primitives changed.

### `src/decoder/metadata.rs`

- `MetadataRevision` separates media-level and per-track containers.
- `StandardTagKey` is removed and replaced by typed `StandardTag` values.
- Existing public metadata fields should retain their behavior unless the task explicitly expands the public API.

### `src/decoder/source.rs`

- `MediaSource` and `MediaSourceStream` now carry explicit lifetimes.
- The crate's owned local-file and HTTP Range sources should be expressible as `'static`; public wrapper types should continue hiding Symphonia details.

## Highest-Risk Semantic Boundary

Symphonia 0.6 enables decoder gapless handling by default and changes where timing and trim data live. A mechanical port could double-trim the beginning or end of a stream. Source inspection found that the 0.6 MP3 and Vorbis decoders consume `AudioDecoderOptions::gapless`; the other bundled audio decoders do not. The implemented policy therefore assigns MP3/Vorbis to the native decoder and all other codecs to the Track-level fallback, with exactly one owner per instance.

## Performance Evidence Boundary

Existing repository performance benchmarks explicitly exclude the decoder, so
this task added the external comparator documented in
`decoder-performance-comparison.md`. It provides same-machine before/after
evidence for borrowed streaming throughput, ABBA-paired raw trial
distributions, and output-frame correctness across WAV, FLAC, Ogg/Vorbis, and
multichannel FLAC.
It does not measure `decode_all` allocation cost or end-to-end playback; those
claims remain out of scope until a compatible benchmark is added.

The gapless-owner comparator is now a version-controlled custom bench at
`benches/audio_gapless_comparison_perf.rs`. Its full run is recorded in
`gapless-owner-comparison-full.json` and the interpretation is documented in
`gapless-owner-comparison.md`. On the available corpus, sequential manual and
native outputs were identical, while Ogg/Vorbis coarse-seek output differed
after decoder reset; FLAC seek output was identical. The report marks the
missing MP3/LAME and CAF fixtures as `skipped`, so those paths remain an
explicit coverage gap rather than an untested pass.

The implemented hybrid policy is verified in
`gapless-hybrid-verification-full.json` and interpreted in
`gapless-hybrid-verification.md`. All four available FLAC/Vorbis fixtures pass
sequential and coarse-seek comparison under `--enforce`; the prior Vorbis
post-reset mismatch is zero after native ownership is selected.

## Recommended Approach

Use a compatibility-first migration:

1. Preserve the crate's public `StreamingDecoder`, builder, source, error, metadata, seek, cancellation, and memory-budget contracts.
2. Adapt internals to Symphonia 0.6 without exposing Symphonia types.
3. Keep the implemented codec allowlist: native MP3/Vorbis trimming and Track fallback for other codecs, with exactly one owner per decoder.
4. Preserve the current codec/format matrix and both default/no-default feature builds.
5. Add measured decoder evidence, but do not combine the dependency migration with a new public chapters/attachments API.
