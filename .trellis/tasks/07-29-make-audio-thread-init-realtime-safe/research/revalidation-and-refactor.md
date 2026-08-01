# Finding 8 revalidation and refactor review

## Current evidence

- `src/runtime.rs:6-18` states that `audio_thread_init` runs on the actual
  callback/playback thread and executes a thread-local once gate.
- The x86/x86_64 implementation only reads and writes thread-local MXCSR; the
  aarch64 implementation only reads and writes thread-local FPCR.
- `src/runtime.rs:67-70` selects a fallback on all other architectures and calls
  `log::warn!`. Logger formatting and dispatch are outside the callback safety
  contract because they may allocate or lock.
- `flush_subnormal_sample` already selects a software finite-subnormal-to-zero
  path on exactly those unsupported architectures.
- Repository search found no control/setup-side caller that needs a capability
  warning, and no production in-repo caller beyond the public API contract;
  current internal call sites are denormal regression tests.
- `src/runtime.rs` was clean before this task, so the finding is not stale and
  there is no overlapping dirty edit to preserve in that file.

## Adopted refactors

1. Apply the supported-architecture cfg to the TLS declaration and the public
   initializer, not only the private register helper. Unsupported targets then
   avoid both logging and pointless TLS access.
2. Keep the supported initializer's once-per-thread behavior and assembly
   unchanged.
3. Keep the unsupported software sample flush as the correctness fallback.
4. Tighten the realtime/logging spec so future edits cannot treat all
   `runtime.rs` logging as setup-only.

## Rejected broader refactors

- Do not change `audio_thread_init` to return support status or `Result`; that
  would be a public API change without an in-repo consumer.
- Do not add a public capability constant/enum merely to preserve the warning.
- Do not initialize or configure a logger; this crate is a library.
- Do not add runtime CPU detection. Architecture-specific register access is
  already compile-time selected and supported behavior is correct.
- Do not create a macro/cfg abstraction for three item selections; it would be
  more indirection than duplication removed.
- Do not change the DSP software flushing policy in this task.

## Expected validation

- Existing x86/x86_64 denormal tests prove supported register setup remains
  effective on the current host.
- Unsupported-target cfg tests should compile and exercise the no-op/software
  fallback whenever the project is built on such a target; no extra cross
  target toolchain is required for this focused fix.
- Source review/search must find no `log::` in `src/runtime.rs`.
- Complete supported feature matrices catch public call-site and cfg drift.

## Final validation evidence

- `cargo test --all-features runtime::tests` passed the supported-target
  idempotence regression on the x86_64 host.
- `cargo test --all-features flushes_denormals` passed all three existing DSP
  denormal regressions.
- `cargo check --lib --no-default-features --features rubato --target
  armv7-linux-androideabi` passed, proving the production unsupported-target
  cfg compiles without the supported-only TLS or register helper.
- `cargo check --all-targets --all-features` passed.
- Strict Clippy passed for both `--all-targets --all-features` and
  `--all-targets --no-default-features --features rubato` with warnings denied.
- The all-features test matrix passed 423 library, 20 benchmark-support, 25
  resampler-support, 3 Windows deployment, and 6 doctests; the native-shim
  prerequisite test was the single expected ignore.
- The Rubato-only test matrix passed 456 library, 20 benchmark-support, 25
  resampler-support, 3 Windows deployment, and 6 doctests; the same native-shim
  prerequisite test was the single expected ignore.
- `cargo fmt --all -- --check`, focused `git diff --check`, and Trellis context
  validation all passed.
- A source search found no `log::` occurrence in `src/runtime.rs`; public call
  sites and signatures remain unchanged.

The final review retained the supported-only TLS/no-op split because it removes
both the realtime violation and needless unsupported-target work. It continued
to reject a public capability API, result-bearing initializer, runtime feature
detection, logger setup, and cfg macro abstraction because none has a current
consumer or reduces enough duplication to justify the added contract surface.
