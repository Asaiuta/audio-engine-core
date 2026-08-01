# Decoder channel-layout boundary revalidation

## Snapshot and verdict

- Revalidated on 2026-07-28 against the current working tree and Symphonia
  0.6.0 dependency source.
- The audit finding is accurate: `layout_from_codec` drops an explicit
  positioned mask whenever any channel is outside its eleven-entry table, then
  guesses every slot from `ChannelLayout::from_count`.
- The finding also affects non-positioned metadata. `Discrete`, `Ambisonic`,
  and `Custom` channel sets currently enter the same conventional-layout
  fallback even though their semantics explicitly are not a conventional bare
  speaker count.

## Dependency evidence

- Symphonia `AudioBuffer` documents positioned planes in least-significant to
  most-significant mask-bit order.
- `Position` includes the existing eleven classified roles plus top/height,
  bottom, wide, and `LFE2` roles.
- `Channels::count()` derives exact geometry for `Positioned`, `Discrete`,
  `Ambisonic`, `Custom`, and `None`.
- `Channels::Custom` preserves the order of its `ChannelLabel` list.
- Both `Channels` and `ChannelLabel` are non-exhaustive, so future variants must
  degrade to `Unspecified`, not to speaker guesses.

## Refactor decision

Adopt a private decoder adapter module with three explicit policies:

1. Dependency metadata owns iteration/order and exact slot count.
2. A small classifier maps only roles the domain model can represent.
3. Count-based inference is a fallback only for absent metadata.

This is preferable to extending the old ordered table. The old table serves as
both iterator and classifier, so an unsupported flag disappears instead of
occupying an unknown slot. Separating these concerns eliminates that failure
mode and makes future Symphonia variants conservatively safe.

Keep the public `ChannelLayout` type dependency-free. Reject adding every
Symphonia position to the public enum in this task: exposing positions without
defined loudness/downmix/rendering policy would enlarge the API while still
leaving ambiguous behavior. Reject changing the downmix architecture; its
existing `Unspecified` zero-gain rule is the correct conservative consumer
contract once the decoder stops fabricating positions.

## Required tests

- Positioned mask with known roles and height roles preserves exact bit order,
  count, and `Unspecified` slots.
- Unsupported positioned bit before `LFE2` does not shift the later supported
  low-frequency role.
- Custom labels preserve order across supported positioned, discrete,
  ambisonic, and unsupported positioned entries.
- Discrete and ambisonic sets become all-`Unspecified` layouts of exact size.
- Missing metadata alone retains `from_count`; explicit `Channels::None`
  remains empty.
- Downmix gives `Unspecified` zero contribution under both coefficient sets,
  and loudness maps it to `ebur128::Channel::Unused`.

## Implemented result

- Added private `src/decoder/channel_layout.rs` as the sole Symphonia-to-domain
  channel adapter. `streaming.rs` no longer owns a supported-only mask table or
  the count-mismatch fallback.
- Positioned layouts iterate Symphonia's mask, so every occupied bit creates a
  slot in canonical interleave order. Classification is a separate function;
  unsupported positions become `Unspecified` in place.
- `LFE1` and `LFE2` both map to the domain-level `LowFrequency` role.
- Custom labels retain declared order. Discrete, ambisonic, unsupported, and
  future label variants remain `Unspecified`.
- Explicit non-positioned metadata derives exact geometry through
  `Channels::count()` and never enters count-based speaker inference. Only an
  absent `Option<Channels>` uses `ChannelLayout::from_count`.
- Added direct downmix and loudness regressions proving unknown slots receive
  neither guessed matrix gain nor R128 weighting.

The module split was adopted because it removes third-party metadata policy
from the streaming lifecycle owner and gives all `Channels` variants one
focused test surface. The public `ChannelPosition` model was not expanded:
height, bottom, wide, custom, and ambisonic rendering require explicit product
policy, and exposing names without defining their DSP semantics would only move
the ambiguity downstream. The public `ChannelLayout::from_count` behavior was
also retained for callers that genuinely have only a bare count.

## Verification result

Verified on 2026-07-28:

- Five focused decoder-adapter tests passed.
- Two focused downstream `Unspecified` behavior tests passed.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `cargo clippy --all-targets --no-default-features --features rubato -- -D warnings`:
  passed.
- `cargo test --all-features`: passed (403 library, 20 benchmark-support,
  25 resampler-support, 3 Windows deployment, and 6 doctests; one native-shim
  prerequisite test ignored by design).
- `cargo test --no-default-features --features rubato`: passed (436 library,
  20 benchmark-support, 25 resampler-support, 3 Windows deployment, and
  6 doctests; the same native-shim prerequisite test ignored by design).
- `cargo fmt --all -- --check`: passed.
- `git diff --check`: passed with only existing LF-to-CRLF checkout notices.
- `task.py validate 07-28-preserve-decoder-channel-layout-semantics`: passed
  with six implementation-context and six check-context entries.

No performance benchmark was run. This metadata adaptation executes during
decoder construction, does not touch the realtime path, and makes no
performance claim.
