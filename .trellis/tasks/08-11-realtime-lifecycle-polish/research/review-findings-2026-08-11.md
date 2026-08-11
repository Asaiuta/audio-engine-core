# Review Findings 2026-08-11 — Realtime Lifecycle

Source: realtime-safety/concurrency deep-review agent report from the
2026-08-11 six-track review. Findings A (drain+fade error loop) and B
(post-fade full-level history flush) were fixed in 1.0.1, along with style
item H (fade restart from unity). Line numbers are pre-1.0.1.

## Verdict context (preserve, do not "improve")

- The hazard-pointer + seqlock parameter snapshot was verified correct by
  interleaving analysis: read-side hazard write → current/sequence re-check →
  dereference; write-side seq-odd → swap → seq-even → retire-scan. All-SeqCst
  was shown **necessary** (store-load ordering between hazard write and
  current re-check; swap vs hazard scan); Acquire/Release is insufficient.
  ABA is closed by the sequence re-check (any free passes through a publish,
  +2 on seq).
- Reclamation runs strictly on the control thread (publish under the control
  mutex); audio only copies `Copy` snapshots. Kernel handoff verified: no
  audio-side drop path for any kernel on any branch of `sync_convolver`
  (convolver.rs:230-373); `try_store_from_audio` returns ownership on CAS
  failure; audio-side kernel count bounded at 3; DropProbe test proves 64/64
  destructions off-audio under concurrent reclaim.
- `assert_no_alloc` is armed via a registered global allocator in test
  builds (lib.rs:100-102); coverage spans full-capacity blocks, first
  adoption on a fresh OS thread, lifecycle requests, terminal finish, and
  concurrent-publish realtime reads.

## Remaining findings

### C (theoretical, low) — O(IR) reset on the audio thread
`pipeline.rs:1391-1396` (in-callback `chain.reset()`) →
`adapters/convolver.rs:528-531` (`owned.kernel.reset()`) →
`convolver.rs:789-808` (partitioned engine `fill`s `input_history_ffts` and
friends, sized by IR length). Million-frame IRs ⇒ tens of MB of writes in
one callback block. No alloc, no lock — but unbounded-in-IR work conflicts
with the crate's spread-quanta philosophy and can miss deadlines on short
buffers. Only manifests with very long IRs + in-callback reset.

### D (theoretical, low) — 40-bit generation wraparound
`pipeline.rs:236-250`: `(generation+1) << 24` silently truncates at 2^40
requests; `take_newer_than` equality could mismatch exactly once at wrap.
~35 years at 1 kHz request rate. Comment-only fix.

### E (theoretical, low) — reset failure still marks request applied
`pipeline.rs:1373-1388`: deliberate anti-retry design, noted in comments.
Currently unreachable (no stage reset returns Err). Leave; revisit only if a
fallible stage reset ever appears.

### F (style, low) — `PeakLimiter::is_enabled()` always true
`limiter.rs:438-441` makes `adapters.rs:1270-1272`'s "enabled changed ⇒
reset" actually mean "any publish while disabled ⇒ reset". The accidental
behavior happens to cover the disable-moment state clear; the code lies.

### G (style, low) — telemetry pseudo-fence
`lockfree_params.rs:1459-1479`: reader's `let _ = factor.load(Acquire)`
orders only against the previous update's Release write; per-field
non-coherence is already the documented contract. Remove the fake fence or
write factor last.

### I (style, low) — latched noise shaper's dead subscription
`adapters.rs:1600-1615`, `1631-1637`: subscribes `params_reader` while
latched, never reads it — occupies a hazard slot, and every publisher
publication scans it.

## Also verified (no action)

`SharedParams::load_if_changed` pointer-equality has no ABA (cached Arc keeps
the allocation alive); `update_if` holds the writer lock across
read-patch-publish with a concurrency regression test; `RingBuffer` is
documented non-RT and unused by the callback path; MXCSR/FPCR bit constants
correct (FTZ 1<<15, DAZ 1<<6, aarch64 FZ 1<<24).
