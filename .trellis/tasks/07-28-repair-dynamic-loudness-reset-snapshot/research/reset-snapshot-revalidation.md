# Dynamic loudness reset snapshot revalidation

## Current behavior

- `DynamicLoudnessProcessor::new` subscribes once and applies cached volume and
  strength to a fresh direct processor.
- `sync_params` only reapplies those controls when
  `load_realtime_if_changed_since` returns a newer generation.
- `DynamicLoudness::reset` clears filter histories, every smoother's current
  value and target, the current loudness factor, applied gains, and active-band
  flags. It retains `strength` but cannot rebuild the factor because it does not
  store the adapter's volume input.
- Adapter reset currently calls only the direct reset and lifecycle reset.
  Its cached snapshot and generation remain unchanged, so ordinary processing
  does not call `set_volume` or `set_strength` again.

## Verdict

The audit finding is accurate and persistent, not a one-block transition.
After reset the control side still reports the published snapshot while the DSP
has zero factor/targets until a later control write changes the generation.

## Refactor decision

Extract `apply_cached_params(&mut self)` on `DynamicLoudnessProcessor` and call
it from construction, changed-generation sync, and reset. This is the smallest
abstraction that removes real duplication and gives snapshot adoption one
owner. Reset must not reread atomics: the adapter's cached snapshot is the
configuration already accepted at a block boundary.

Do not add volume storage to `DynamicLoudness`; that would duplicate adapter
control ownership and change a public direct-DSP contract to solve an adapter
lifecycle bug. Do not recreate the direct processor on reset because that risks
drifting constructor/sample-rate/control setup. Telemetry can update on the
next processed block as before and is not part of the persistent divergence.

## Required proof

- Publish non-unity volume and strength once.
- Advance one adapter so filter/smoother history differs from fresh state.
- Reset it without another publication and assert cached generation retention.
- Compare its next output and direct control-derived state bit-for-bit with a
  newly constructed adapter subscribed to the same snapshot.
- Assert the reset path allocates nothing and remains accepted by the shared
  lifecycle driver.

## Implemented repair

`DynamicLoudnessProcessor::apply_cached_params` is now the single owner of the
adapter-snapshot-to-direct-DSP mapping. Construction, changed-generation sync,
and reset all call it. Reset clears the direct signal and smoother state first,
reapplies the already-adopted cached volume and strength, and then resets the
fixed lifecycle. The cached generation remains unchanged because no new atomic
snapshot was accepted.

The regression test publishes volume `0.05` and strength `0.37` once, advances
one adapter through prior-stream audio, resets it inside `assert_no_alloc`, and
compares its next stream and control-derived DSP state bit-for-bit with a fresh
adapter subscribed to the same snapshot. The shared processor lifecycle test
also covers reset and resumed processing for the adapter.

## Final verification

- Focused reset regression: passed with all features and with Rubato only.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `cargo clippy --all-targets --no-default-features --features rubato -- -D warnings`: passed.
- `cargo test --all-features --no-fail-fast`: 408 library, 20 benchmark-support,
  25 resampler-support plus 1 explicit ignored native-shim test, 3 Windows, and
  6 doctests passed.
- `cargo test --no-default-features --features rubato --no-fail-fast`: 441
  library, 20 benchmark-support, 25 resampler-support plus 1 explicit ignored
  native-shim test, 3 Windows, and 6 doctests passed.
- `cargo fmt --all -- --check`, focused `git diff --check`, and Trellis task
  validation passed after the task evidence was finalized.

## Final refactor review

The private helper is retained because it removes a real duplicate mapping and
names the adapter's snapshot-adoption responsibility. No physical module split
is warranted for one adapter-local mapping. Adding volume storage to the direct
DSP, reconstructing the entire processor, forcing an atomic reload, or
redesigning telemetry were rejected because each introduces another state owner
or changes boundaries unrelated to the defect.
