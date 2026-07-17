# Backend Spec: audio-engine-core

> Source-backed conventions for this Rust audio-core crate. These files describe
> what the code **actually does**, not generic ideals. The crate is an
> app-agnostic library of decoder / DSP / loudness / resampling / pipeline
> primitives — there is no server, UI, or business-logic layer here.

---

## Read This First

[**Realtime Safety**](./realtime-safety.md) is the most important invariant in
the crate. Any code that runs inside an audio callback or the DSP chain must
obey its hot-path prohibitions (no alloc/lock/log/IO/panic/unbounded work).

## Spec Index

| Guide | Description |
|-------|-------------|
| [Realtime Safety](./realtime-safety.md) | The hot-path invariant: what is forbidden in an audio callback and how parameters cross the thread boundary. **Start here.** |
| [Streaming DSP Lifecycle](./streaming-lifecycle.md) | Object-safe block/progress, backpressure, finish/reset, latency/tail, timing, and error contracts for processors and chains. |
| [DSP State Correctness](./dsp-state-correctness.md) | Stateful branch ownership, config publication, RBJ coefficient oracles, and sample-rate update boundaries. |
| [AutoMix & FIR Correctness](./analysis-fir-correctness.md) | Spectral cadence/tempo, explicit key capability, FIR absolute gain/phase windows, and FIR benchmark evidence. |
| [Directory Structure](./directory-structure.md) | Live `src/` layout: `decoder/`, `processor/`, `processor/loudness/`, benches, examples, and where new code goes. |
| [Error Handling](./error-handling.md) | `DecoderError` / `NetworkError` model via `thiserror`, `?` propagation, and the no-panic-on-callback rule. |
| [Logging Guidelines](./logging-guidelines.md) | `log`-facade conventions for non-RT paths; logging is forbidden on the hot path. |
| [Quality Guidelines](./quality-guidelines.md) | The evidence policy, versioned benchmark/baseline/CI contracts, forbidden/required patterns, and review checklist. |
| [Database Guidelines](./database-guidelines.md) | The optional `loudness-db` SQLite cache only — not a business DB; never on the realtime path. |

## Cross-References

- `realtime-safety.md` is referenced by `logging-guidelines.md`,
  `quality-guidelines.md`, `database-guidelines.md`, and `error-handling.md`.
- The evidence policy in `quality-guidelines.md` is derived from
  `.trellis/tasks/06-12-audio-engine-feature-upgrade/research/current-algorithm-audit.md`.
- Authoritative project docs that these specs must not contradict:
  `README.md`, `CONTRIBUTING.md`, `NOTICE`, `CLAUDE.md` / `AGENTS.md`.

**Language**: All spec documentation is written in **English**.
