# Logging Guidelines

> Logging conventions for the **non-realtime** paths of this crate. The hot
> audio path does not log at all — see `realtime-safety.md`.

---

## Library, Not Application

This crate uses the `log` crate facade (`log = "0.4"`) and emits records
through `log::warn!` / `log::info!` / etc. It does **not** initialize a logger
or pick a backend — choosing and installing a logger (`env_logger`,
`tracing-log`, etc.) is the consuming application's job. Never add a logger
implementation or global init here.

## Where Logging Is Allowed

Logging is allowed only on non-RT, setup/decode/diagnostic paths. Current call
sites are all off the audio callback:

- `decoder/streaming.rs`, `decoder/source.rs`, `decoder/error.rs` — decode and
  network-retry diagnostics.
- `decoder/metadata.rs` — metadata extraction.
- `pipeline.rs`, `runtime.rs`, `diagnostics.rs` — pipeline/runtime setup.
- `processor/resampler.rs`, `processor/loudness/normalizer.rs`,
  `processor/loudness/meter.rs` — setup/control-path diagnostics, not the
  per-sample inner loops.

## Where Logging Is Forbidden

**No `log::*` macro may appear on the hot path** (`dsp_chain.rs`,
`adapters.rs`, and the per-sample processor loops). These files contain zero
`log::` calls and must stay that way; a log call inside a callback can allocate,
format, and lock, violating realtime safety. If you need visibility into a
processor's behavior, surface it through a value the control thread reads (e.g.
an atomic telemetry snapshot like `AtomicDynamicLoudnessTelemetry`), not a log
line.

## Log Levels

- `warn!` — a recoverable problem the caller should know about (e.g. a network
  attempt failed and will be retried; a fallback path was taken).
- `info!` — significant lifecycle events on setup/decode paths.
- `debug!` / `trace!` — detailed diagnostics, off by default in release.
- `error!` — reserve for genuine failures; in a library prefer returning a
  typed error (see `error-handling.md`) over logging at `error!` and continuing.

## What Not To Log

- Nothing on the realtime path, ever.
- No tight-loop / per-sample logging on any path (floods and skews timing).
- No secrets; for HTTP sources, do not log full credentials/tokens.
