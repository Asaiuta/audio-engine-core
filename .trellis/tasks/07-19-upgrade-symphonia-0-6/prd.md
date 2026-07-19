# Upgrade Symphonia to 0.6

## Goal

Upgrade the decoder backend from Symphonia 0.5.5 to the latest stable 0.6.0 release while preserving `audio-engine-core` public behavior, decode correctness, feature-gated builds, bounded memory, and gapless/seek semantics. Capture measured evidence for any performance or binary-size claim rather than assuming an upstream improvement transfers to this crate.

## What I Already Know

- `Cargo.toml` currently requests Symphonia `0.5` with `features = ["all"]`; `Cargo.lock` resolves 0.5.5.
- Symphonia 0.6.0 is the latest stable release as of 2026-07-19 and requires Rust 1.85. The local toolchain is Rust 1.93.1; existing crate code uses APIs stabilized in Rust 1.87.
- Direct Symphonia usage is localized to `src/decoder/streaming.rs`, `src/decoder/source.rs`, and `src/decoder/metadata.rs`, but those files touch most of the APIs redesigned in 0.6.
- Current decoder tests synthesize deterministic PCM WAV fixtures and cover opening, borrowed output, fixed staging bytes, cancellation, typed failures, seek, truncation, and memory limits.
- Existing repository performance benches exclude decoding; this task adds a
  task-local, same-machine decoder comparator so the upgrade claim is backed by
  raw streaming measurements rather than upstream release notes alone.
- Symphonia 0.6 does not materially expand the audio codec list; its strongest benefits are safety, probe/demux correctness, metadata, default SIMD, and ongoing maintenance.

## Assumptions

- The existing public `StreamingDecoder` family and `AudioInfo` behavior should remain source-compatible unless a 0.6 constraint makes that impossible.
- The crate should declare Rust 1.87 as its effective MSRV: this is higher than Symphonia's 1.85 requirement because existing DSP code already uses 1.87-stabilized APIs.
- The migration is compatibility-first: richer 0.6 chapters, attachments, and typed metadata are not exposed as new public APIs in this task.
- HTTP Range/full-download behavior remains behind the existing `http` feature.

## Requirements

- Upgrade the dependency and lockfile to Symphonia 0.6.0 with the current codec, format, metadata, and SIMD capabilities intentionally selected.
- Migrate format probing, format-reader EOF handling, audio decoder registry construction, optional codec parameters, track timing, and seek units to the 0.6 APIs.
- Replace removed `SampleBuffer`/`SignalSpec` usage while preserving interleaved `f64` output, borrowed decode output, `decode_next_into`, `decode_next`, and `decode_all` contracts.
- Preserve fixed decoder staging accounting and enforce the configured memory limit before unbounded decoded-output growth.
- Migrate metadata extraction while preserving existing title, artist, album, album artist, genre, track/disc, date, comment, and artwork behavior where the source provides them.
- Ensure gapless delay/padding is applied exactly once at true stream start/end and is not re-applied after seek.
- Make gapless ownership an explicit codec policy: Symphonia owns trimming for
  MP3 and Vorbis, whose 0.6 decoders consume packet trim/reset state; the
  crate-owned Track fallback remains active for codecs that ignore the native
  option. Never enable both owners for one decoder instance.
- Preserve typed `DecoderError` mapping, cancellation, local source behavior, and optional HTTP source behavior.
- Preserve the supported audio format/codec matrix and document any unavoidable difference.
- Add focused regression coverage for every changed semantic boundary.
- Add decoder-specific before/after performance evidence or an explicit limitation if a portable compatible baseline cannot be produced.

## Acceptance Criteria

- [x] `Cargo.toml` and the local ignored `Cargo.lock` resolve Symphonia 0.6.0 without retaining 0.5.x Symphonia packages.
- [x] The public decoder API continues to compile for existing repository callers.
- [x] Deterministic decode tests prove channel count, sample rate, interleaved sample count, finite samples, and expected PCM values.
- [x] `decode_next_borrowed` retains stable decoder-owned storage semantics without steady-state allocation growth.
- [x] Staging-memory accounting remains exact and configured decoder memory limits are enforced.
- [x] Empty, garbage, truncated, no-audio-track, canceled, and unsupported inputs retain typed/non-panicking behavior.
- [x] Seek tests preserve the documented coarse tolerance and do not re-trim encoder delay.
- [x] Gapless tests prove no double trim and correct final frame count.
- [x] The hybrid owner policy selects native MP3/Vorbis and Track fallback for
  other codecs, and the real Ogg/Vorbis seek comparison passes against the
  native reference.
- [x] Existing metadata fields remain correct for covered fixtures; the 0.6 metadata model does not silently discard known tags/artwork.
- [x] Local files and HTTP Range/full-download source paths continue to work under their feature configurations.
- [x] `cargo test --lib` passes.
- [x] `cargo test --lib --no-default-features` passes.
- [x] `cargo clippy --all-targets -- -D warnings` passes, with a no-default-features strict check if supported by the existing workflow.
- [x] `cargo fmt --check`, `cargo doc --no-deps`, and release/package checks required by backend quality guidelines pass.
- [x] Decoder-specific release evidence is recorded with raw trials, compiler/target/features, workload hashes, output validation, and limitations; no unsupported end-to-end playback claim is added.

## Definition of Done

- Implementation and regression tests are complete.
- Default, no-default, documentation, lint, and package verification gates are green.
- Decoder performance evidence is recorded or the lack of comparable evidence is explicitly documented.
- README/CHANGELOG/MSRV documentation reflects observable changes without unsupported claims.
- Trellis specs are reviewed for any new durable decoder conventions.

## Technical Approach

Use a compatibility-first internal migration. Keep Symphonia types behind the decoder module boundary, adapt the new lifetime/audio/metadata primitives internally, and preserve the crate-owned `f64` interleaved surface. Treat gapless ownership and fixed staging memory as explicit design decisions rather than mechanical API substitutions. Validate output correctness before evaluating performance.

## Decision (ADR-lite)

**Context:** Symphonia 0.6 provides safety, metadata, default SIMD, and decoding improvements but redesigns nearly every upstream API used by this crate.

**Decision:** Perform a bounded decoder-module refactor that preserves the current public surface and behavior. Use an internal hybrid gapless policy: native Symphonia trimming for MP3/Vorbis and Track-level fallback for other codecs, with exactly one owner per instance. Do not expose new chapters/attachments/video-oriented APIs in the same task. Add evidence before claiming performance improvement.

**Consequences:** The change is larger than a dependency bump and records the crate's effective Rust 1.87 MSRV without unrelated DSP rewrites. The result avoids long-term 0.5 API debt while limiting downstream churn. New 0.6-only public metadata features can be designed separately after the migration stabilizes.

The user's request is specifically to start the dependency upgrade. The conservative scope therefore treats new public metadata capabilities as future evolution rather than silently expanding this task.

## Out of Scope

- Adding Opus or another new codec backend.
- Adding video or subtitle support.
- Replacing Symphonia with FFmpeg, GStreamer, or another decoder.
- Exposing chapters, attachments, or a redesigned public metadata model.
- Refactoring unrelated DSP, resampling, loudness, pipeline, or database modules.
- Making unsupported end-to-end playback performance claims from decoder-only measurements.

## Research References

- [`research/symphonia-0-6-migration.md`](research/symphonia-0-6-migration.md) - official release/migration findings and their mapping to this repository.
- [`research/gapless-owner-comparison.md`](research/gapless-owner-comparison.md) - native-vs-manual gapless correctness and timing comparison on the available Ogg/Vorbis and FLAC corpus.
- [`research/gapless-hybrid-verification.md`](research/gapless-hybrid-verification.md) - enforced post-change verification of the codec-aware owner policy.

## Technical Notes

- Relevant specs: `.trellis/spec/backend/error-handling.md`, `.trellis/spec/backend/directory-structure.md`, `.trellis/spec/backend/quality-guidelines.md`, and `.trellis/spec/backend/realtime-safety.md` where decoder buffers cross into callback-owned flows.
- Highest-risk files: `src/decoder/streaming.rs`, `src/decoder/source.rs`, `src/decoder/metadata.rs`, and `src/decoder/tests.rs`.
- Likely release-facing files: `Cargo.toml`, `Cargo.lock`, `README.md`, and `CHANGELOG.md`.
