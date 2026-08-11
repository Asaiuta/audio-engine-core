# Replace String errors with typed error boundaries

1.0 release gate 3 of 9.

## Goal

Kill every `Result<_, String>` in the public API and (candidate) in `src/`
before 1.0 freezes them, so callers can distinguish error classes (cancellation,
decoder, I/O, lock, database) instead of matching on message text. This closes
audit P2 #4 (AutoMix) and P2 #7 (LoudnessDatabase) — both deliberately left
open — plus the AutoMix key-DTO contradiction (P3).

## What I already know

### Inventory of `Result<_, String>` sites (verified 2026-08-03)

**Public API — `LoudnessDatabase` (`src/processor/loudness_db.rs`, feature
`loudness-db`, default-on): 12 public + 1 private method**

| Line | Signature | Error sources observed |
|---|---|---|
| 196 | `open(path) -> Result<Self, String>` | rusqlite, io |
| 220 | `in_memory() -> Result<Self, String>` | rusqlite |
| 234 | `init_schema() -> Result<(), String>` (private) | rusqlite |
| 287 | `upsert(track) -> Result<(), String>` | mutex poison, rusqlite |
| 329 | `get(path) -> Result<Option<TrackLoudness>, String>` | mutex, rusqlite |
| 372 | `get_fresh(path) -> ...` | mutex, rusqlite |
| 397 | `needs_scan(path) -> Result<bool, String>` | mutex, rusqlite |
| 455 | `get_outdated_tracks() -> Result<Vec<String>, String>` | mutex, rusqlite (row errors already propagate via `collect::<Result<..>>` — audit's "silently drops" is stale) |
| 472 | `batch_upsert(tracks) -> Result<usize, String>` | mutex, rusqlite |
| 526 | `set_album_gain(...) -> Result<(), String>` | mutex, rusqlite |
| 549 | `delete(path) -> Result<bool, String>` | mutex, rusqlite |
| 564 | `stats() -> Result<DatabaseStats, String>` | mutex, rusqlite |

**Public API — AutoMix (`src/processor/automix_analysis.rs`):**

| Line | Signature | Erased classes |
|---|---|---|
| 383 | `analyze_automix(...) -> Result<AutomixAnalysis, String>` | decoder open/decode, seek/IO (via DecoderError), cancellation |
| 392 | `analyze_automix_with_cancel(...) -> ...` | same |

**Internal (pub(super)/private):**

| Site | Signature | Note |
|---|---|---|
| `automix_analysis.rs:473` | `decode_segment(...) -> Result<(), String>` | maps DecoderError to String |
| `automix_analysis.rs:543` | `check_cancel(...) -> Result<(), String>` | cancellation erased |
| resampler backends (`contiguous_polyphase` :197-221, `halfband` :55-58, `polyphase` :49-73/:276, `spectral` :78-102, `rubato` :1272) | constructors `Result<_, String>` (2 sites also `Result<_, &'static str>`) | geometry validation, pub(super), non-RT setup |
| `decoder/streaming.rs:452,462` | maps codec error → `DecoderError::Decoder(String)` | **typed envelope already** — String is the payload |
| `decoder/source/http.rs:262` | `invalid_range(String)` → `NetworkError::InvalidRangeResponse(String)` | **typed envelope already** — String is the payload |

### Existing typed errors (the convention to extend, not replace)

- `ProcessError` (`processor/traits.rs:557`) — rich, `#[non_exhaustive]`,
  `#[from] AudioBlockError`/`TimingError`, structured variants. No String payloads.
- `DecoderError` (`decoder/error.rs`) — `#[from] io::Error → FileOpen`,
  feature-gated `Network`, `UnsupportedFormat` (defined, never constructed),
  generic `Decoder(String)` / `Probe(String)` payloads.
- `NetworkError` (http feature) — fully typed; `InvalidRangeResponse(String)` payload.
- Spec: `.trellis/spec/backend/error-handling.md` — "errors are returned as
  typed `Result<T, E>` values", "Do not stringify an error early if a typed
  variant exists", `#[from]` only for lossless conversions, feature-gated
  variants stay behind `#[cfg]`.

### Downstream consumer (AudioPlayer, separate repo)

Re-exports `processor::{...}` wholesale (`AudioPlayer/src/lib.rs:28-29`):
`analyze_automix`, `AutomixAnalysis`, `AutomixAnalysisOptions`,
`LoudnessDatabase`, `DatabaseStats` are all imported; `player/gapless.rs` and
`player/loading.rs` use `LoudnessDatabase` directly. Every public signature
change here breaks the downstream on its next `cargo update` — recorded as a
follow-up (gate 2 precedent), not fixed in this repo.

### The AutoMix key DTO (P3)

`AutomixKeyStatus` (public, `#[non_exhaustive]`) has only `Unsupported`, while
`AutomixAnalysis` publicly exposes four optional key payload fields
(`:45-88`); finalization always emits `Unsupported` + four `None`s
(`:510-515`). Audit recommends a smaller capability/result model *when a
detector exists* — the reservation is premature surface.

### Already handled / not in scope

- `get_outdated_tracks` row errors already propagate (stale audit claim).
- `Decoder(String)` / `Probe(String)` payloads: typed envelope exists; replacing
  the payload semantics is owned by the decoder-format-capability work per
  error-handling.md ("Known gap ... do not treat as the intended contract").
- `NetworkError::InvalidRangeResponse(String)`: payload, not class.

## Decisions

### Scope — public + internal (2026-08-03, user choice A)

Every `Result<_, String>` in `src/` production code is converted, public and
internal alike. Checkable criterion: zero `Result<_, String>` / failure
`Result<_, &'static str>` in `src/` (tests may keep local helpers).

### Error architecture — per-module enums, include public ResamplerError
(2026-08-03, user choice B)

- `LoudnessDatabaseError` (public, new): `CreateDirectory(#[from] io::Error)`,
  `Database(#[from] rusqlite::Error)`, `LockPoisoned` — sources verified at
  `loudness_db.rs:196-260` (io only in `open`; rusqlite everywhere;
  `Mutex<Connection>` poison).
- `AutomixError` (public, new): `Canceled` + `Decoder(#[from] DecoderError)` —
  open/decode/seek/IO classes stay distinguishable through the chain
  (`automix_analysis.rs:400,436,487` all map `DecoderError`).
- `ResamplerError` (existing public, restructured): replace
  `InitializationFailed(String)` / `ProcessFailed(String)` with typed variants.
  Construction sites: `mod.rs:227` invalid sample rate, `:235` zero channels,
  `:418` streaming geometry (`channels/from_rate/to_rate`), `:432` input
  capacity overflow, `:254` backend init (per-channel), `:298-346` runtime
  process/drain/stall/out-of-bounds diagnostics. Backend geometry errors
  become a `pub(crate) BackendInitError` (ZeroChannels, RatioExceedsLimit{..},
  CoefficientCountOverflow, CoefficientBankTooLarge{..},
  NonlinearPhaseRequired, EmptyMinimumPhaseFactor, InvalidChunkSize{..},
  InvalidGeometry) mapped into the flattened `ResamplerError` variants at the
  facade; runtime diagnostics keep a `message: String` payload on otherwise
  typed variants (crate convention, cf. `DecoderError::Decoder(String)`).
- `Decoder(String)`/`Probe(String)` payloads and
  `NetworkError::InvalidRangeResponse(String)`: unchanged (out of scope).

### AutoMix key DTO — shrink now (2026-08-03, user choice 1)

`AutomixAnalysis` drops the four reserved key payload fields
(`key_root: Option<i32>`, `key_mode: Option<i32>`, `key_confidence: Option<f64>`,
`camelot_key: Option<String>`, `:101-115`) and the `AutomixKeyStatus` enum
(only variant `Unsupported`) — zero consumers in-repo and downstream.
Audit-aligned: introduce a capability/result model only when a detector
actually exists. Finalization at `:641-645` and serialized JSON shape change
are downstream follow-up material.

## Open Questions

1. ~~**Scope**: public + internal~~ → decided: option A
2. ~~**Error architecture**~~ → decided: option B (per-module enums, public
   `ResamplerError` restructured)
3. ~~**P2-7 DTO**~~ → decided: shrink now (remove 4 payload fields +
   `AutomixKeyStatus`)
4. ~~**Downstream**~~ → recorded as follow-up (gate 2 precedent): AudioPlayer
   re-export list + any `LoudnessDatabase`/`analyze_automix`/`AutomixAnalysis`
   call sites and serialized JSON shape update in one future sync.

## Requirements

* Add a public, `#[non_exhaustive]` `LoudnessDatabaseError` and return it from
  every `LoudnessDatabase` operation. Preserve I/O and rusqlite errors as
  sources and expose mutex poisoning as its own stable class instead of
  leaking a poison-message string.
* Add a public, `#[non_exhaustive]` `AutomixError` with distinct cancellation
  and decoder variants. Preserve `DecoderError` as the source so decoder,
  seek, network, and I/O policy remains available to callers.
* Remove `AutomixKeyStatus` and the four reserved optional key fields from
  `AutomixAnalysis`. A real detector may introduce a coherent capability and
  result model later; this task does not reserve a contradictory placeholder.
* Restructure public `ResamplerError` into stable typed initialization and
  processing classes. Invalid facade geometry, checked-capacity failures,
  backend initialization failures, and runtime processing/drain failures must
  remain distinguishable without callers parsing display text.
* Replace bare backend `String` and `&'static str` error returns with internal
  typed envelopes. Backend initialization uses structured variants for known
  geometry and coefficient failures; runtime failures retain diagnostic text
  only as a payload inside a typed processing class.
* Keep successful decoding, analysis, database, and resampling behavior
  unchanged. This task changes failure contracts and removes unused AutoMix
  key placeholders; it does not redesign decoder payload errors or add key
  detection.

## Acceptance Criteria

* [x] Every public `LoudnessDatabase` method returns
      `Result<_, LoudnessDatabaseError>`; tests distinguish database, I/O, and
      poisoned-lock classes without matching display strings.
* [x] `analyze_automix` and `analyze_automix_with_cancel` return
      `Result<AutomixAnalysis, AutomixError>`; tests match cancellation and a
      decoder failure as separate variants and verify the decoder source is
      retained.
* [x] `AutomixKeyStatus`, `key_root`, `key_mode`, `key_confidence`, and
      `camelot_key` are absent from the public surface, implementation,
      serialization tests, and benchmark consumers.
* [x] `ResamplerError` no longer exposes the catch-all
      `InitializationFailed(String)` / `ProcessFailed(String)` pair. Focused
      tests match typed zero-rate, zero-channel, invalid-geometry, capacity,
      backend-initialization, and processing-failure classes.
* [x] No production Rust source under `src/` returns a bare
      `Result<_, String>` or failure `Result<_, &'static str>`; existing String
      payloads inside the out-of-scope typed decoder/network envelopes remain.
* [x] Existing success-path unit and integration tests remain green for both
      `--all-features` and `--no-default-features --features rubato`.
* [x] `CHANGELOG.md` records the public breaking changes and both checked-in
      public-API snapshots are refreshed as reviewable diffs.

## Technical Approach

Use `thiserror` for the public module-owned enums and lossless `#[from]`
conversions only. Database lock poisoning maps explicitly to
`LoudnessDatabaseError::LockPoisoned`; it does not retain the guard-dependent
`PoisonError` type. AutoMix propagates `DecoderError` with `?` and maps token
state directly to `AutomixError::Canceled`.

The resampler facade owns the public classification. Backends return crate-only
typed initialization and processing errors; the facade maps these into
`ResamplerError` while adding channel/backend/operation context where useful.
Known validation failures use data-bearing variants, while diagnostics that
cannot be made more semantic without changing third-party backends remain a
message field on a typed backend/processing variant.

## Decision (ADR-lite)

**Context:** Gate 1 made public-surface changes reviewable, and Gate 2 removed
legacy exports. Gate 3 must now stabilize failure classes before 1.0 without
pretending every third-party backend diagnostic has a durable semantic model.

**Decision:** Use public per-module error enums, crate-private typed resampler
backend errors, and remove the unused AutoMix key reservation now.

**Consequences:** This is an intentional pre-1.0 breaking change. Callers gain
variant-based policy and source chains, but AudioPlayer must update imports,
matches, and any serialized AutoMix shape when it next refreshes this crate.

## Definition of Done

* Tests updated for every converted site
* Clippy / fmt / full matrix green on both feature sets
* CHANGELOG breaking-change entry
* Public-API baselines regenerated (pure diff)
* Spec updates (error-handling.md, streaming-lifecycle.md if touched)
* Downstream follow-up recorded

## Out of Scope (explicit)

* Decoder `String` payloads (`Decoder(String)` / `Probe(String)`) — owned by
  decoder-format-capability work
* Adding an AutoMix key detector / real key states
* Any behavior change to successful paths

## Technical Notes

* Prior audit: `archive/2026-08/07-28-codebase-maintainability-audit/research/`
  `03a-dsp-and-analysis-modules.md` (P3 key DTO), `03b-decoder-and-runtime-modules.md`
  (P2 loudness), `08-p2-reverification-and-remediation.md` (P2 #4 residual).
* `.trellis/spec/backend/error-handling.md` is the governing spec.
