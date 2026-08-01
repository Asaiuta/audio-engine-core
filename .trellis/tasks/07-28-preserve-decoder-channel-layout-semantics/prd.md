# Preserve decoder channel layout semantics

## Goal

Prevent explicit Symphonia channel metadata from being discarded and replaced
with a count-based conventional speaker guess. Preserve every interleaved slot,
retain the positions this crate can classify, and represent unsupported,
discrete, ambisonic, or otherwise unknown roles as `Unspecified`.

## Revalidation verdict

The 2026-07-28 maintainability finding is accurate and broader than its height-
channel example. `layout_from_codec` currently collects only eleven positioned
flags; if any other flag exists, it discards the complete mask and calls
`ChannelLayout::from_count`. The same fallback also guesses speaker roles for
Symphonia 0.6 `Discrete`, `Ambisonic`, and `Custom` metadata.

Symphonia documents that a `Positioned` buffer follows ascending mask-bit order.
Its current `Position` mask includes height, bottom, wide, and a second LFE role
beyond the eleven entries handled by the crate. `Custom` preserves the supplied
label order. These dependency contracts make a complete, ordered adapter
possible without guessing.

## Requirements

- A positioned mask drives iteration in ascending mask-bit/interleave order.
- Every mask bit produces exactly one `ChannelLayout` slot.
- Existing supported speaker positions retain their current mapping.
- `LFE1` and `LFE2` both map to the crate's role-level `LowFrequency` position.
- Unsupported positioned roles map to `Unspecified` in place; they never cause
  known slots to be discarded or relabelled.
- Custom labels preserve their declared order, mapping supported positioned
  labels and emitting `Unspecified` for all other labels.
- Discrete, ambisonic, `None`, future unknown channel-set variants, and custom
  labels the crate cannot classify never receive guessed speaker roles.
- Count-based conventional inference remains available only when the container
  supplies no `Channels` metadata at all.
- For explicit metadata, the layout count derives from that metadata rather
  than a separate caller-supplied count.

## Refactor scope

Move the Symphonia-to-domain adapter from `streaming.rs` into a private focused
decoder module. Keep the public, dependency-free `ChannelLayout` primitive in
`src/channel_layout.rs` unchanged. This isolates third-party metadata semantics
from streaming lifecycle code and gives the adapter a focused test surface.

Do not expand the public `ChannelPosition` enum to model every immersive-audio
role, redesign downmix/loudness policy, or change the public decoder API. Those
would require product-level policy for rendering height, wide, ambisonic, and
custom channels; guessing those semantics inside a correctness fix would
recreate the problem at a larger boundary.

## Acceptance Criteria

- [x] Front L/R plus height channels remain Front L/R plus `Unspecified` slots
      in exact interleave order and count.
- [x] A later supported bit such as `LFE2` remains correctly aligned after an
      earlier unsupported positioned bit.
- [x] Custom labels preserve order and known/unknown slot identity.
- [x] Discrete and ambisonic channel sets are entirely `Unspecified`, not
      conventional count-based layouts.
- [x] Missing channel metadata still uses the existing count-based fallback.
- [x] Downmix and loudness behavior for `Unspecified` remains conservative:
      no guessed surround gain or R128 contribution.
- [x] Focused tests, both supported Clippy/test matrices, rustfmt, diff check,
      and Trellis context validation pass.
- [x] Final review records the adopted module/refactor boundary and the broader
      public model changes intentionally rejected.

## Definition of Done

- The mapping implementation has one dependency-driven interleave iterator and
  one role-classification function; it does not duplicate a supported-only
  ordered mask table.
- Regression tests cover positioned, custom, discrete, ambisonic, absent, and
  explicitly empty metadata.
- Decoder/channel-layout contracts are captured in the backend spec.
- Existing unrelated dirty work remains untouched.
- No commit, push, or archive occurs without the user's explicit direction.

## Out of Scope

- Rendering or downmix policy for height, bottom, wide, ambisonic, or arbitrary
  custom channels.
- A public lossless wrapper around every Symphonia channel label.
- Changing the existing best-effort `ChannelLayout::from_count` API for callers
  that intentionally possess only a count.
