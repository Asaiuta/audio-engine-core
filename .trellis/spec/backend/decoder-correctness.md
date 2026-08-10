# Decoder Correctness

> Executable contracts for adapting codec/container metadata into the crate's
> decoder domain types and planning bounded full-decode storage. Transport
> errors remain in `error-handling.md`; callback safety remains in
> `realtime-safety.md`.

## Scenario: Typed Media Location Owns Source Routing

### 1. Scope / Trigger

- Trigger: changing `MediaLocation`, `HttpMediaLocation`, decoder source
  opening, AutoMix source inputs, or a downstream identity derived from a
  media source.
- A path-like string cannot preserve native local paths and also prove that an
  HTTP URL is validated. The enum variant is therefore the routing decision.

### 2. Signatures

```rust
pub enum MediaLocation {
    Local(PathBuf),
    Http(HttpMediaLocation),
}

impl MediaLocation {
    pub fn local(path: impl Into<PathBuf>) -> Self;
    pub fn http(input: impl AsRef<str>) -> Result<Self, MediaLocationError>;
}

impl HttpMediaLocation {
    pub fn parse(input: impl AsRef<str>) -> Result<Self, MediaLocationError>;
    pub fn from_url(url: url::Url) -> Result<Self, MediaLocationError>;
    pub fn url(&self) -> &url::Url;
}

StreamingDecoder::open(location: MediaLocation) -> Result<StreamingDecoder, DecoderError>
analyze_automix(location: MediaLocation, ...) -> Result<AutomixAnalysis, AutomixError>
```

`url` is a direct dependency because these public types and constructors exist
in local-only builds. The `http` feature gates transport, not representation.

### 3. Contracts

- Construct local and HTTP locations explicitly. No public combined-source
  entry point guesses a variant from a string or path prefix.
- `Local(PathBuf)` reaches `File::open` without UTF-8 conversion or mandatory
  canonicalization. A lossy rendering may be used only as a diagnostic label,
  never for routing, opening, or cache identity.
- `HttpMediaLocation` keeps its `Url` private and accepts only `http` or
  `https` URLs with a host. Callers cannot construct an invalid HTTP variant.
- Source opening matches the enum variant. With `http` disabled, an HTTP
  variant returns `DecoderError::FeatureUnavailable`; it is never retried as a
  local path.
- HTTP request code receives the already-validated full URL. `Debug`,
  `Display`, and library logs use the origin-only `log_identity`; path, query,
  fragment, and userinfo never reach library-controlled diagnostics.
- AutoMix and staged/direct decoder opening consume the same typed location so
  routing, credentials, cancellation, and redaction cannot drift by caller.

### 4. Validation & Error Matrix

| Input / build | Required result |
| --- | --- |
| Arbitrary native local `PathBuf` | `MediaLocation::Local`; byte-exact local open |
| Mixed-case `HTTP://` or `HTTPS://` input | parsed and normalized by `url`, then routed as HTTP |
| `ftp://...` | `MediaLocationError::UnsupportedScheme` |
| HTTP URL without a host | `MediaLocationError::MissingHost` |
| Malformed URL | `MediaLocationError::InvalidUrl` with parse source |
| HTTP location in a no-`http` build | `DecoderError::FeatureUnavailable` |
| URL containing credentials and signed components | full URL is used for the request; only origin is formatted |

### 5. Good / Base / Bad Cases

- Good: construct `MediaLocation::http`, validate once, and pass that value
  through playback, AutoMix, and cache identity construction.
- Base: construct `MediaLocation::local` without touching the filesystem; a
  later open reports any I/O failure.
- Bad: call `to_string_lossy`, inspect `http://` prefixes, reparse the value in
  each consumer, or expose `MediaLocation::Http(url::Url)` directly.

### 6. Tests Required

- On Unix, create a non-UTF-8 filename and prove staged and direct local opens
  decode it without classification or byte loss.
- Cover mixed-case schemes, malformed URLs, unsupported schemes, missing
  hosts, and the no-HTTP `FeatureUnavailable` branch by typed variant.
- Assert `Debug`, `Display`, and log identity omit username, password, private
  path, signed query, and fragment while `url()` retains the request URL.
- Compile and test all-features and Rubato-only matrices; run both public API
  snapshots after changing any signature or re-export.

### 7. Wrong vs Correct

#### Wrong

```rust
let text = path.as_ref().to_string_lossy();
if text.starts_with("http://") || text.starts_with("https://") {
    open_http(&text)
} else {
    File::open(text.as_ref())
}
```

#### Correct

```rust
match location {
    MediaLocation::Local(path) => open_local_media_source(path),
    MediaLocation::Http(http) => open_validated_http(http),
}
```

## Scenario: Channel Metadata Preserves Slot Identity

### 1. Scope / Trigger

- Trigger: changing `AudioInfo::channel_layout`, Symphonia `Channels`/
  `ChannelLabel` handling, `ChannelPosition`, or downstream layout-aware
  loudness/downmix behavior.
- The boundary is correctness-sensitive: a guessed speaker role changes R128
  weighting and downmix gain. An unknown role is safer than a confidently wrong
  role.

### 2. Signatures

```rust
pub(super) fn layout_from_codec(
    channels: Option<&symphonia::core::audio::Channels>,
    fallback_count: usize,
) -> ChannelLayout;

pub struct AudioInfo {
    pub channels: usize,
    pub channel_layout: ChannelLayout,
    // ...
}
```

### 3. Contracts

- `None` means the container supplies no channel metadata. Only this branch may
  call `ChannelLayout::from_count(fallback_count)`.
- `Channels::Positioned` iterates the dependency mask itself. Symphonia defines
  this as least-significant to most-significant mask-bit order, which is also
  its buffer/interleave order. Every bit produces one output slot.
- The domain classifier maps Front L/R/C, LFE1/LFE2, Rear L/R/C, Side L/R, and
  Front L/R Center. Every other positioned role becomes `Unspecified` in place.
- `Channels::Custom` preserves label-list order. A supported single positioned
  label is classified; discrete, ambisonic, unsupported positioned, composite,
  and future label variants become `Unspecified`.
- Discrete, ambisonic, explicit `Channels::None`, and future non-positioned
  channel-set variants derive exact geometry from `Channels::count()` and never
  receive conventional speaker guesses.
- `AudioInfo.channels == AudioInfo.channel_layout.channel_count()` for metadata
  produced by decoder construction.
- `AudioInfo` is observation, not control. `StreamingDecoder::info` and
  `StreamingDecoderBuilder::info` return `&AudioInfo`; neither exposes a mutable
  field. The decoder trusts these same values for staging geometry, gapless
  trimming, buffer sizing, seek arithmetic, and reported position, so a
  caller-supplied edit would be an unvalidated control channel into decode
  state. Tests that need synthetic gapless counters — WAV fixtures carry none —
  use the crate-private `set_gapless_counters_for_test`, which names exactly the
  two overridable fields and is unreachable outside the crate.
- `Unspecified` has zero downmix contribution under every coefficient set and
  maps to `ebur128::Channel::Unused`; adding a new rendering policy requires an
  explicit public-domain model change and new downstream tests.

### 4. Validation & Error Matrix

| Input metadata | Required layout |
| --- | --- |
| Absent metadata plus count 4 | conventional count-based four-channel layout |
| Front L/R plus two height bits | Front L/R plus two `Unspecified` slots |
| Unsupported bit before LFE2 | `Unspecified` followed by `LowFrequency`; no slot shift |
| `Discrete(4)` | four `Unspecified` slots |
| first-order `Ambisonic(1)` | four `Unspecified` slots |
| Custom Front R, discrete, height, Front L | preserve that order as Front R, unknown, unknown, Front L |
| Explicit `Channels::None` | empty layout, regardless of fallback count |
| Future non-exhaustive variant | exact `count()` slots, all `Unspecified` unless deliberately supported |

### 5. Good / Base / Bad Cases

- Good: dependency metadata owns iteration and geometry; a separate classifier
  decides only whether each occupied slot has a representable domain role.
- Base: metadata is absent, so a caller-visible best-effort conventional layout
  is inferred from the bare count.
- Bad: iterate only a list of supported roles, observe a count mismatch, discard
  the explicit mask, and guess all speakers from the count.
- Bad: map discrete or ambisonic channels to Front/Rear roles because their
  total happens to be 2, 4, 6, or 8.

### 6. Tests Required

- Adapter unit tests assert exact ordered slices for mixed known/unsupported
  positioned masks and custom labels.
- Assert a supported position after an unsupported bit retains its original
  slot, proving unknown roles are not merely appended.
- Assert discrete and ambisonic layouts contain only `Unspecified` and have the
  exact dependency-reported count.
- Assert absent metadata retains conventional inference while explicit empty
  metadata remains empty.
- Downmix tests place non-zero samples only in unknown slots and assert zero
  stereo output under ITU and ATSC coefficient sets.
- Loudness tests assert `Unspecified -> ebur128::Channel::Unused`.
- Run both strict Clippy and test matrices: all-features and
  `--no-default-features --features rubato`.

### 7. Wrong vs Correct

#### Wrong

```rust
let known = SUPPORTED_POSITIONS
    .iter()
    .filter(|(flag, _)| mask.contains(*flag))
    .map(|(_, role)| *role)
    .collect::<Vec<_>>();
if known.len() != channel_count {
    return ChannelLayout::from_count(channel_count);
}
```

#### Correct

```rust
let positions = mask
    .iter() // dependency-defined canonical interleave order
    .map(|position| classify_or_unspecified(position))
    .collect::<Vec<_>>();
ChannelLayout::from_positions(positions)
```

## Scenario: Full Decode Plans Storage Before Mutation

### 1. Scope / Trigger

- Trigger: changing `StreamingDecoder::decode_all`, decoded packet append,
  `DECODE_MAX_MEMORY_MB`, `DecodeMemoryBudget`, or a consumer that derives an
  allocation from decoded frame/channel metadata.
- Container duration and publicly visible channel geometry are not trusted
  allocation sizes. Every conversion, multiplication, reservation, and append
  must stay behind the same checked boundary.

### 2. Signatures

```rust
pub fn decode_memory_budget() -> DecodeMemoryBudget;

pub struct DecodeMemoryBudget {
    pub limit_mb: usize,
    pub limit_bytes: usize,
    pub source: &'static str,
}

pub fn decode_all(&mut self) -> Result<Vec<f64>, DecoderError>;
```

The private implementation uses one decoded-buffer size plan carrying both the
validated interleaved sample count and its `f64` byte count. It is not a public
geometry type.

### 3. Contracts

- Convert metadata frames with `usize::try_from`, then use `checked_mul` for
  frames by channels and samples by `size_of::<f64>()`. Reuse the resulting
  sample/byte pair for the budget check, reservation, and diagnostics.
- The effective budget is the requested/default MiB value clamped by both the
  configured 64..32768 MiB interval and `isize::MAX` bytes for the target. The
  latter is Rust's maximum single-`Vec` allocation size. A simulated 32-bit
  target therefore resolves both the 2048 MiB default and 32768 MiB override to
  2047 whole MiB.
- A successfully parsed environment override keeps
  `source == "DECODE_MAX_MEMORY_MB"` after clamping. A missing or unparsable
  value keeps `source == "default"`.
- Initial capacity uses `try_reserve_exact`; allocation/capacity failure maps
  to `DecoderError::Decoder`. Do not call infallible `Vec::with_capacity` with
  untrusted metadata.
- Each packet is obtained from `decode_next_borrowed`. Compute checked
  `destination.len() + packet.len()` samples and bytes, compare the final size
  to the budget, and reserve fallibly before `extend_from_slice`. A rejected
  packet leaves the destination unchanged.
- The HTTP non-Range full-download path consumes its existing fraction of the
  same resolved budget. Target normalization must propagate to that consumer
  without changing its transport policy.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| `u64` frame count cannot convert to `usize` | `DecoderError::Decoder`; no reservation |
| frames by channels or samples by width overflows | `DecoderError::Decoder`; no reservation |
| checked initial bytes exceed the budget | typed file-too-large error before allocation |
| fallible initial/incremental reservation fails | `DecoderError::Decoder`; no append |
| checked current plus packet bytes exceed the budget | typed memory-limit error; destination unchanged |
| duration is absent | start with zero planned capacity and enforce every packet incrementally |
| valid size is within target/configured bounds | decode and preserve existing gapless/output behavior |

### 5. Good / Base / Bad Cases

- Good: one plan derives 128 frames * 6 channels = 768 samples = 6144 bytes,
  and every downstream decision reuses those values.
- Base: unknown duration starts empty; each borrowed packet passes checked
  growth, budget, and reservation preflight before append.
- Bad: cast duration with `as usize`, repeat ordinary multiplication in the log
  and allocation, or check `Vec::len()` only after `extend_from_slice`.
- Bad: clamp only to a `usize` byte maximum. `Vec` capacity is limited by
  `isize::MAX`, which is one byte below 2048 MiB on a 32-bit target.

### 6. Tests Required

- Pure size-plan tests assert exact ordinary samples/bytes and reject
  `u64::MAX`, channel multiplication overflow, and sample-width overflow.
- A pure budget-resolver test injects `i32::MAX` as the allocation ceiling and
  asserts default/maximum configuration both become 2047 MiB with the correct
  `source` field.
- Append preflight tests retain a copy of the destination and assert it is
  byte-for-byte unchanged after an over-budget packet.
- Decoder-focused tests cover known and unknown duration behavior without
  changing borrowed staging, gapless, seek, or HTTP fallback semantics.
- Run both strict Clippy and test matrices: all-features and
  `--no-default-features --features rubato`.

### 7. Wrong vs Correct

#### Wrong

```rust
let samples = frames as usize * channels;
let mut output = Vec::with_capacity(samples);
decode_next_into(&mut output)?;
if output.len() * size_of::<f64>() > max_bytes {
    return Err(limit_error()); // allocation and mutation already happened
}
```

#### Correct

```rust
let initial = DecodedBufferSizePlan::from_frames(frames, channels)?;
ensure_within_budget(initial.bytes, max_bytes)?;
output.try_reserve_exact(initial.samples)?;

while let Some(packet) = decoder.decode_next_borrowed()? {
    let next = DecodedBufferSizePlan::after_append(output.len(), packet.len())?;
    ensure_within_budget(next.bytes, max_bytes)?;
    output.try_reserve_exact(packet.len())?;
    output.extend_from_slice(packet);
}
```
