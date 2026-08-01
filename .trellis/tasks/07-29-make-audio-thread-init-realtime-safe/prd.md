# Make audio-thread initialization realtime-safe

## Goal

Remove architecture-dependent logging and unnecessary thread-local work from
`audio_thread_init` so every compiled implementation is safe to call from the
actual audio callback. Preserve hardware FTZ/DAZ initialization on supported
architectures and the existing software subnormal fallback elsewhere.

## Revalidation Verdict

Audit finding 8 is accurate in the current tree. `audio_thread_init` is
documented for the actual callback/playback thread, but its fallback
`set_audio_thread_float_mode` calls `log::warn!` on every architecture other
than x86, x86_64, and aarch64 before marking the thread-local flag initialized.
Formatting and logger dispatch may allocate or lock on the realtime path.

The finding also exposes avoidable work: an unsupported target enters the
thread-local once gate even though hardware setup is a no-op. The existing
`flush_subnormal_sample` already provides the per-sample software fallback on
those targets, so the public initializer can compile directly to a no-op there.

## Requirements

- Keep `pub fn audio_thread_init()` source-compatible and idempotent.
- On x86, x86_64, and aarch64, retain the thread-local once gate and current
  hardware register behavior.
- Compile the thread-local initialization flag only on architectures that use
  it.
- On unsupported architectures, compile `audio_thread_init` as a direct,
  allocation-free, lock-free, logging-free no-op.
- Preserve `flush_subnormal_sample`: supported architectures return the sample
  directly; unsupported architectures zero finite subnormal values in
  software.
- Preserve debug/test `audio_thread_float_mode_is_enabled` behavior: initialize
  then report the hardware bits on supported architectures and `false`
  elsewhere.
- Do not add a public capability API, result value, callback telemetry, logger
  setup, or architecture abstraction without a current consumer.

## Acceptance Criteria

- [x] No `log::*` call remains in `src/runtime.rs` or the
      `audio_thread_init` call graph.
- [x] Supported architectures still initialize once per actual thread and the
      existing denormal tests pass.
- [x] Unsupported architectures compile a direct no-op initializer without
      referencing the TLS flag, while retaining software subnormal flushing.
- [x] Public signatures and current call sites remain unchanged.
- [x] Both complete feature/test and strict Clippy matrices, rustfmt, focused
      diff check, and Trellis validation pass.
- [x] Final review records adopted and rejected broader refactors.

## Definition of Done

- Every target-specific `audio_thread_init` body satisfies the realtime
  no-allocation/no-lock/no-log contract.
- The realtime and logging specs no longer imply that `runtime.rs` logging is
  categorically safe.
- Existing unrelated dirty work is preserved.
- No commit, push, or archive occurs without explicit user direction.

## Decision (ADR-lite)

**Context**: Removing the fallback warning fixes the immediate defect, but
leaves unsupported targets paying a TLS access for an operation that can never
change hardware state. Adding a new capability API would preserve a diagnostic
with no in-repo control-side consumer.

**Decision**: Use compile-time architecture selection at the public initializer
and TLS declaration. Supported targets retain the once-only register update;
unsupported targets use an inline no-op and the existing software sample
flush.

**Consequences**: Callers keep the same API and supported-target behavior.
Unsupported targets lose a callback-unsafe warning and avoid TLS work. A future
application that needs a user-visible capability notice can add a control-side
API when an actual consumer and contract exist.

## Out of Scope

- Changing MXCSR/FPCR bit selection or inline assembly.
- Replacing the TLS once gate on supported architectures.
- Moving `audio_thread_init` calls into an application/device layer.
- Adding runtime target detection or a public capability enum.
- Refactoring processor subnormal handling or DSP algorithms.

## Technical Notes

- Primary code: `src/runtime.rs`.
- Existing callers/tests are processor denormal regressions plus external
  consumers of the public initializer.
- Contracts: `.trellis/spec/backend/realtime-safety.md` and
  `.trellis/spec/backend/logging-guidelines.md`.
- Revalidation and refactor decisions:
  `research/revalidation-and-refactor.md`.
