# Decoder and Format Capability Upgrades

## Goal

Make the Symphonia-based decode path's format support, seek, and error behavior explicit and testable, so supported vs unsupported inputs produce well-defined, asserted results instead of ambiguous failures. Scope was set by a source-backed capability audit (see Research References), not by guessing which formats to add.

## Requirements

### 1. Typed error path is real and tested

- Today `DecoderError::UnsupportedFormat` is defined (error.rs:139) but never constructed: probe/decode failures return generic `DecoderError::Probe(String)` / `Decoder(String)` (streaming.rs:50,97). Route genuinely unsupported/garbage inputs to the specific typed variant. (`NoAudioTrack` is already wired at streaming.rs:63 via `ok_or`, but has no test coverage.)
- Cover with tests: an unsupported/garbage input yields `UnsupportedFormat` (or `Probe` with a documented reason), and a container with no audio track yields `NoAudioTrack` — not a generic stringly error or panic. Both paths are currently untested.

### 2. Seek accuracy + fix the post-seek gapless double-trim

- Add a seek test that asserts the post-seek sample position is within a defined tolerance (Coarse mode has bounded inaccuracy; document the tolerance rather than claiming sample-exact).
- Fix the audited bug: `seek()` resets `samples_output = 0` (streaming.rs:298), so after seeking to a non-zero position the `decode_next_into` delay-trim logic (streaming.rs:187-196) re-trims `encoder_delay` frames from the post-seek stream. Encoder-delay trimming must only apply at true stream start, not after an arbitrary seek.
- Decide and document whether `SeekMode::Accurate` is offered; if it stays Coarse-only, state that explicitly as the supported contract.

### 3. Format test matrix + corrupt-input policy

- Add a format → capability test matrix (decode + metadata assertions) over small fixtures or synthetic sources for the formats we commit to supporting.
- Define and test the behavior for corrupt/truncated input: a defined, documented outcome (typed error, not a panic or silent empty success).

### Cross-cutting

- Keep Symphonia as the base decoder; do not replace it. The crate already compiles `symphonia = { features = ["all"] }`, so the work is making behavior explicit/tested, not adding codecs.
- Preserve realtime safety: decode-side allocation is acceptable; `decode_next_into` must remain allocation-free in steady state (it already reuses `sample_buf`).
- Update README/format-support text only after the corresponding tests pass.

## Acceptance Criteria

- [ ] Unsupported input returns `UnsupportedFormat` (or documented `Probe` reason); a no-audio-track container returns `NoAudioTrack` — both asserted by tests, neither panics.
- [ ] A seek test asserts post-seek sample position within a defined, documented tolerance.
- [ ] Post-seek decode does NOT re-trim `encoder_delay`; a regression test covers seek-to-nonzero followed by position check.
- [ ] A format → capability matrix test covers decode + metadata (sample rate, channel count, duration) for each committed-supported format against known fixtures.
- [ ] Corrupt/truncated input has a defined, tested policy (typed error, no panic, no silent empty pass).
- [ ] `cargo build --no-default-features` and `--features http` both pass (capability coherent with/without `http`).
- [ ] README/format-support text updated only after the above tests pass.

## Validation Commands

- `cargo test decoder --lib`
- `cargo test --lib`
- `cargo build --no-default-features`
- `cargo build --no-default-features --features http`
- `cargo clippy --all-targets -- -D warnings`

## Out of Scope

- Replacing Symphonia or adding a custom codec implementation.
- Adding new codecs/containers beyond what `features = ["all"]` already compiles in.
- Channel layout exposure, downmix/upmix policy (owned by `06-12-audio-engine-channel-layout-mixing`). This task surfaces only the existing `channels: usize` count; positional layout is the sibling task.
- DSP quality changes (EQ, loudness, saturation, convolution).
- Device output, CPAL/WASAPI ownership, or network transport beyond the existing HTTP source.

## Research References

- [`research/decoder-capability-audit.md`](research/decoder-capability-audit.md) — source-backed audit of current decode/seek/metadata/error behavior; flags the seek double-trim bug and the untyped error paths.

## Technical Notes

- HTTP streaming decode is gated behind the `http` feature (Range requests + full-download fallback); capability behavior must be coherent with and without that feature.
- Audited facts (verified against source, correcting an earlier sub-agent report): gapless delay/padding trimming runs inside the streaming `decode_next_into` (streaming.rs:187-211), NOT only in `decode_all` — `decode_all` just loops it. The earlier audit claim that streaming consumers get untrimmed output is wrong.
- Seek currently uses `SeekMode::Coarse` only (streaming.rs:293).
- **Execution-order dependency**: `06-12-audio-engine-trellis-spec-bootstrap` must be implemented first. It creates `realtime-safety.md` and fills the placeholder backend spec files. Until then, the RT-safety reference in `implement.jsonl` points at `logging-guidelines.md` (where the hot-path rule temporarily lives); migrate it to `realtime-safety.md` once bootstrap lands. `task.py validate` rejects jsonl entries whose files do not yet exist, so the reference cannot point at `realtime-safety.md` before bootstrap creates it.
- Source anchors: `src/decoder/streaming.rs`, `src/decoder/source.rs`, `src/decoder/metadata.rs`, `src/decoder/tests.rs`, `src/decoder/error.rs`.
- Shared audit: `../06-12-audio-engine-feature-upgrade/research/current-algorithm-audit.md`.
