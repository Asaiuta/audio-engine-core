# Public API and control-boundary audit

## Snapshot and scope

- Evidence was re-read after the concurrent `src/pipeline.rs` update at
  2026-07-28 14:59:58 +08:00 (101,451 bytes).
- Final source re-read for this area: 2026-07-28 15:01:22 +08:00.
- Focused validation after that update:
  `cargo test --all-features pipeline::playback_facade_tests` passed all 29
  matching tests at 15:01 +08:00.
- Stable supporting files in this area: `src/processor/lockfree_params.rs`,
  `src/processor/traits.rs`, `src/processor/adapters.rs`, and
  `src/processor/resampler/mod.rs`.
- The full-suite results in `00-scope-inventory-and-baseline.md` predate the
  14:59 pipeline edit. They remain historical evidence, not validation of the
  exact pipeline snapshot reviewed here.

Reviewed boundaries:

- crate-root facade exports in `src/lib.rs`;
- `PlaybackConfig`, `PlaybackParameters`, `PlaybackController`, and
  `PlaybackPipeline` in `src/pipeline.rs`;
- public atomic parameter publishers in
  `src/processor/lockfree_params.rs`;
- the public `StreamingProcessor` contract and every production
  `set_enabled` implementation.

## Verdict

This API is not an unstructured facade: its callback/control ownership model,
strict build-time validation, lock-free lifecycle channel, and pre-registered
realtime snapshot readers all have explicit invariants and tests. However, the
current public surface still has two correctness-level boundary breaks and
several interface/naming inconsistencies that make successful calls less
trustworthy than their types and documentation imply.

## Confirmed findings

### P1 — `set_eq_band_gain_db` reports success for an invalid band

**Category**: correctness defect; false-success error boundary.

Evidence:

- `src/pipeline.rs:693-698` returns `Result<(), ProcessError>`, validates only
  that `gain_db` is finite, delegates the band index, and then always returns
  `Ok(())`.
- `src/processor/lockfree_params.rs:500-509` silently returns without
  publishing when `band >= EQ_BANDS`.
- Current tests cover non-finite and out-of-range gains only with band `0`
  (`src/pipeline.rs:2228` and `:2292`); there is no invalid-index test.

Consequence:

An integration can persist or display a successful EQ edit that never took
effect. The lower layer's no-op policy is hidden behind a high-level fallible
API, so the `Result` cannot be trusted as an acknowledgement.

Direction:

Validate the index at the facade boundary and return a typed error, or make the
low-level setter itself return an outcome that the facade must propagate. Do
not retain `Result` while silently discarding one class of invalid input.

### P1 — one saturation gain update publishes two observable snapshots

**Category**: correctness defect; incoherent composite publication.

Evidence:

- `PlaybackParameters` promises complete snapshot updates at callback block
  boundaries (`src/pipeline.rs:652-660`).
- `set_saturation_gains_db(input, output)` is one semantic operation, but
  `src/pipeline.rs:781-795` calls `set_input_gain` and `set_output_gain`
  separately.
- Each low-level setter performs its own locked read-modify-publish
  (`src/processor/lockfree_params.rs:791-810`).
- A complete coherent saturation publisher already exists at
  `src/processor/lockfree_params.rs:686-733`.
- No current test calls `set_saturation_gains_db`; the focused 29-test facade
  suite therefore cannot detect an intermediate mixed pair.

Consequence:

If the callback reaches a block boundary between the two control-thread
publishes, it can observe the new input gain with the old output gain for one
or more blocks. Paired makeup gains are commonly chosen together, so the mixed
state can create an avoidable level discontinuity.

Direction:

Add one low-level atomic `set_gains`/snapshot-update operation that holds the
writer serialization boundary once and publishes once. A facade-side
`read`-then-`write` is insufficient because it would introduce stale-snapshot
lost updates against concurrent control publishers.

### P2 — `StreamingProcessor::set_enabled` is mandatory but not universally meaningful

**Category**: unclear abstraction boundary; interface-segregation and contract
substitutability defect.

Evidence:

- The public trait requires `set_enabled` and documents it as "Enable or
  transparently bypass this processor" (`src/processor/traits.rs:689-701`).
- `VolumeProcessor::set_enabled` is an intentional silent no-op and directs
  callers to a different, non-trait operation (`src/processor/adapters.rs:1499-1580`).
- `StreamingResampler::set_enabled` is also a silent no-op because rate
  conversion is graph geometry (`src/processor/resampler/mod.rs:1128-1202`).
- `VolumeProcessor::is_enabled` always returns true, whereas the resampler's
  value means "rates differ", not an enable flag.

Consequence:

Generic code using `dyn StreamingProcessor` can call the trait operation under
its documented bypass contract and receive neither a bypass nor an error. The
same method and predicate represent three different concepts: effect bypass,
always-on gain stage, and graph-rate geometry.

Direction:

Separate streaming lifecycle from optional bypass capability, or make the
operation report unsupported/capability explicitly. The resampler should not
be forced to pretend graph geometry is a mutable effect switch.

### P2 — `AtomicDynamicLoudnessParams::set_ref_volume_db` can lose concurrent updates

**Category**: concurrency boundary defect in a public low-level API.

Evidence:

- `SharedParams::update` holds the control-writer mutex while it reads,
  modifies, and publishes (`src/processor/lockfree_params.rs:337-349`).
- `set_ref_volume_db` instead reads a whole snapshot before acquiring the
  publisher lock, modifies it, and later publishes it
  (`src/processor/lockfree_params.rs:1268-1280`).
- An interleaving `set_strength`/`set_enabled` can publish after the first read
  and then be overwritten by the stale whole snapshot.
- Repository search found no production call site; only the pointer-stability
  unit test at `src/processor/lockfree_params.rs:1450-1459` uses this method.

Consequence:

The type is public and designed for cloned control publishers, but this one
partial setter does not preserve other fields under concurrent writers. Its
current internal non-use reduces immediate product impact, not the public
contract inconsistency.

Direction:

Provide an update-if-changed primitive that performs the comparison and
mutation inside the writer serialization boundary.

### P3 — snapshot readers use positional tuples and one is materially incomplete

**Category**: inaccurate/weak naming; maintainability smell.

Evidence:

- `crossfeed() -> (bool, f64, f64)` at `src/pipeline.rs:904-908`;
- `saturation() -> (bool, f64, f64, f64)` at `:909-920`;
- `dynamic_loudness() -> (bool, f64, f64)` at `:921-929`;
- `noise_shaping() -> (bool, u32, NoiseShaperCurve)` at `:931-935`.

The positional `f64` values are distinguishable only by documentation and
destructuring order. More importantly, the broad name `saturation()` returns
only enabled/drive/threshold/mix while writable saturation state also includes
type, quality, input/output gains, high-pass mode/cutoff, and arming. Its phrase
"as applied by the callback" is also too strong: the method reads the latest
control-side publication, which may not yet have been consumed at a callback
block boundary.

Direction:

Use named public state/readback structs and distinguish `published` intent
from callback-acknowledged/applied state. If only a summary is desired, encode
that in the method/type name.

### P3 — `PlaybackController` partially duplicates `PlaybackParameters` without a stated rule

**Category**: blurry ownership/ergonomic boundary.

Evidence:

- The controller documentation says ordinary DSP controls belong on the
  cloneable handle returned by `parameters()` (`src/pipeline.rs:948-970`).
- It nevertheless proxies `set_volume`, `set_muted`, and
  `dynamic_loudness_telemetry` at `src/pipeline.rs:1011-1022`.
- Every other ordinary parameter requires `controller.parameters()`; lifecycle
  and convolver authority remain controller-only.

Consequence:

Callers have two ordinary-control entry paths, but only for an unexplained
subset. Future additions require deciding by precedent rather than by a stable
capability rule, and docs/tests can drift between the two paths.

Direction:

Either keep the controller focused on non-cloneable lifecycle/convolver
authority, or document and consistently apply a small, explicit convenience
policy for direct proxies.

### P3 — fade duration is classified as interleaved-buffer geometry

**Category**: inaccurate error naming.

Evidence:

- An over-limit `fade_ms` returns `ProcessError::InvalidGeometry` at
  `src/pipeline.rs:995-1001`, and the unit test codifies that variant at
  `:2087-2094`.
- That variant's public message is specifically "invalid interleaved geometry"
  (`src/processor/traits.rs:592-599`).
- `ProcessError::InvalidParameter` already represents rejected control values
  at `src/processor/traits.rs:605-612`.

Consequence:

Logs and integrations receive an error category whose wording does not match
the rejected value, making diagnostics and programmatic handling less precise.

## Important non-findings / justified complexity

### Lifecycle ownership is now reachable and explicit

The older facade required `&mut PlaybackPipeline` for reset/drain after the
pipeline had moved into the callback. The current snapshot fixes that boundary:

- `LifecycleChannel` packs kind, fade payload, and generation into one atomic
  word (`src/pipeline.rs:209-273`), avoiding a torn multi-field command.
- `PlaybackController::{request_reset,request_drain,request_stop_with_fade}`
  publish control-thread requests, and `PlaybackPipeline::process` consumes at
  most one coalesced request at a block boundary.
- Focused tests cover pre-first-block requests, coalescing, fade, drain/idle,
  reset, terminal requests, and allocation-free callback handling.

The packed channel and lifecycle state machine add complexity, but they solve a
real ownership constraint without putting locks or allocation on the callback.
They are not over-design on current evidence.

### The custom realtime snapshot mechanism is justified

`src/processor/lockfree_params.rs:108-349` keeps control-side `ArcSwap`
convenience while realtime readers use pre-registered hazard slots and copy
`Copy` snapshots. Replaced ownership is reclaimed by the publisher, not by the
audio thread. The concurrent-publication allocation test passes in both
feature matrices in the baseline. This is a complex mechanism backed by the
crate's strongest realtime invariant, not complexity to remove merely because
it is longer than a conventional atomic/lock wrapper.

### Strict initial config versus clamped runtime control is explicit

`PlaybackConfig::validate` rejects invalid presets before DSP construction,
whereas finite runtime slider values are clamped and non-finite values are
rejected. Current tests cover both policies and readback of applied bounds.
That two-policy distinction is documented and coherent; it is not duplicate
validation by accident.

## Superseded observations

The following problems existed in an earlier moving snapshot but are not
current findings:

- non-finite facade and low-level parameter writes now preserve the previous
  snapshot and have tests;
- EQ composite writes now store the same clamped gains the DSP applies;
- saturation enable/drive and other armed runtime controls no longer always
  return `UnsupportedOperation`;
- lifecycle reset/drain is no longer unreachable from the control thread.

## Test gaps exposed by this review

The current focused facade suite is green but has no test for:

- an EQ band index equal to or greater than `EQ_BANDS`;
- one coherent callback snapshot for `set_saturation_gains_db`;
- a concurrent `set_ref_volume_db` versus another partial dynamic-loudness
  update;
- generic disable behavior across every production `StreamingProcessor`
  implementation.

These are gaps in evidence, not an instruction to modify tests during this
read-only audit.

