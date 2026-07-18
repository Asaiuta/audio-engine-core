# Convolver Module Split Options

## Current shape

`src/processor/adapters.rs` is 2,926 lines / roughly 104 KiB. Its production sections are:

* shared fixed-stage lifecycle and validation helpers;
* seven ordinary parameter-snapshot adapters (EQ, Saturation, Crossfeed, PeakLimiter,
  Volume, NoiseShaper, DynamicLoudness);
* roughly 580 lines of Convolver control, ownership, telemetry, state machine, and trait
  implementation;
* one test module of roughly 1,338 lines, including thirteen Convolver tests in two distant
  groups.

The new AtomicPtr design adds the highest-risk unsafe ownership proof, a consumer lease,
versioned quiescence, and finish locking. Leaving those inside the existing file would make
review harder precisely where the task needs a narrow audit boundary.

Public consumers do not import private implementation files. `processor::mod` and `lib.rs`
curate `ConvolverControl`, `ConvolverProcessor`, and `ConvolverStatus`, while
`output_chain.rs` imports them from `super::adapters`. A private submodule plus re-export from
`adapters` therefore preserves the intended public surface without exposing new modules or
adding a compatibility facade.

## Option 1 - Targeted Convolver vertical module (recommended)

Use the existing Rust convention of a module root plus subdirectory:

```text
src/processor/
├── adapters.rs                         # shared helpers + seven ordinary adapters + re-exports
└── adapters/
    ├── tests.rs                        # non-Convolver adapter tests
    ├── convolver.rs                    # ConvolverProcessor RT state machine
    └── convolver/
        ├── handoff.rs                  # private AtomicPtr Box ownership primitive + safety proof
        ├── control.rs                  # control/status/consumer lease/quiescence
        └── tests.rs                    # Convolver ownership/lifecycle/adversarial tests
```

`adapters.rs` declares a private `mod convolver` and publicly re-exports only the existing
curated types. `convolver.rs` owns the audio state machine and declares private `handoff` and
`control` children. The low-level unsafe module exposes only role-appropriate fixed-slot
operations to its parent; all `Box::from_raw` sites and Drop/shutdown invariants are kept in
that small file. Convolver tests are descendants of the module so they can exercise private
invariants without making test hooks public.

Move the remaining existing adapter tests to `adapters/tests.rs` mechanically. This brings
the production root back near one thousand lines without rewriting the other seven adapters.

Advantages:

* Unsafe ownership, control-plane policy, and audio state machine have distinct review
  boundaries.
* Convolver tests sit beside the private protocol and no longer remain split across a large
  unrelated test module.
* The other adapters are not redesigned; their implementation and public paths stay stable.
* `adapters.rs` becomes a readable shared-adapter root rather than a control protocol and test
  archive.

Costs:

* Requires careful visibility (`pub(super)` / private) and import movement.
* Moving tests creates a moderate mechanical diff, though production behavior is unchanged.
* Shared test helpers may need small local replacements rather than broadening production
  visibility.

## Option 2 - Extract only the AtomicPtr hand-off helper

Create one `convolver_handoff.rs` for unsafe pointer operations but leave status, control,
processor state machine, and all tests in `adapters.rs`.

Advantages:

* Smallest file movement and a narrow unsafe file.

Costs:

* `adapters.rs` remains a roughly 2,800-line god module after the new lease/finish/telemetry
  logic is added.
* Control and audio ownership policy remain interleaved with seven unrelated adapters.
* Does not satisfy the structural finding beyond the most obvious unsafe block.

## Option 3 - Split every adapter into its own module

Turn `adapters` into a full directory and move all eight processors, shared helpers, and tests
to per-adapter files.

Advantages:

* Most uniform long-term layout and smallest individual files.

Costs:

* Large mechanical churn across adapters unrelated to this defect.
* More opportunity for visibility, import, and test drift while the P0 ownership rewrite is
  already high risk.
* Violates the task's explicit boundary against a general adapter-framework rewrite.

## Recommendation

Choose Option 1. It removes the risky Convolver vertical slice and all inline tests from the
god module while leaving the seven unaffected production adapters in place. Keep public paths
stable through normal module re-export, not a deprecated compatibility layer. Update the
directory-structure and realtime/streaming specs after implementation to document the new
ownership boundary.
