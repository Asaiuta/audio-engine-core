# Current Quality and Performance Gate Gap Audit

## Scope

This audit compares the current custom benchmark harnesses and CI workflow with
the parent task's evidence requirements. It intentionally avoids proposing a
single audio-quality score or cross-machine absolute timing threshold.

## Existing foundations

### Objective quality

`benches/audio_quality_measurements.rs` already provides:

* `gate`, `report`, and `skipped` metric classification;
* `--enforce` with metric/measured/threshold diagnostics;
* `--out` JSON;
* explicit skipped EBU loudness and true-peak corpora when vectors are absent;
* synthetic resampler, limiter, saturation, EQ, crossfeed, dynamic-loudness,
  noise-shaping, loudness, and full-output true-peak probes.

Its JSON conditions describe algorithms but not the build/runtime environment.
Full-output points expose output frame count but discard the authoritative
`RenderedOutput` latency, semantic-tail, rendered-frame, and truncation fields.

### Realtime performance

`audio_callback_chain_perf` and `audio_resampler_streaming_perf` both:

* use deterministic synthetic input and explicit warmup;
* cover multiple buffer sizes and scenarios/API paths;
* validate successful processing (the resampler also validates approximate
  output duration);
* accept quick/full/heavy modes and a limited `--enforce` flag.

Both currently choose the fastest trial. They do not retain the distribution,
report median/p95/max, emit JSON, capture environment metadata, compute deadline
utilization, or compare a candidate with a stored compatible baseline.

`audio_convolver_perf` and `audio_lockfree_params_perf` already demonstrate the
repo's preferred machine-robust pattern: same-run relative comparisons with
conservative thresholds.

### CI

`.github/workflows/ci.yml` covers format, two strict Clippy configurations,
docs, cross-platform all/no-default tests, and package verification. It runs no
custom quality/performance benchmark and uploads no benchmark evidence.

## P0 regression coverage matrix

| Parent defect/probe | Current regression evidence | Remaining gate work |
| --- | --- | --- |
| SoXR input loss / drain / reset leakage | `short_and_long_integer_upsampling_consume_every_frame`, `random_input_chunking_matches_single_feed`, `finish_is_terminal_idempotent_and_reset_clears_native_history` | Preserve output-work validation in resampler perf JSON |
| Last-frame impulse and downstream finalize | `default_render_compensates_limiter_latency_and_preserves_last_impulse`, `convolver_tail_flows_through_limiter_and_resampler_independent_of_block_size` | Publish `RenderedOutput` length/latency/tail/truncation in quality JSON |
| EQ target-state loss | `transition_adopts_complete_target_state_for_tone_and_impulse`, irregular chunk and no-allocation tests | Reference the tests in the final evidence matrix; do not duplicate |
| Loudness config default leakage | `constructor_publishes_disabled_album_config_and_bypasses`, all-mode round trip | Reference the tests; no new synthetic quality metric needed |
| RBJ shelf duplicated sine | `test_cached_geometry_coefficients_match_rbj_reference` | Keep corrected dynamic-loudness objective metric classified as a gate |
| Dynamic-loudness rate rebuild loses controls | direct and adapter `sample_rate_change_preserves_*` tests | Reference the tests; no timing gate needed |
| Unknown/infinite tail termination | block-size-independent stop, exact cap and truncation tests | Publish truncation metadata and retain explicit policy conditions |

The P0 mechanisms already have strong unit/property coverage. This task should
not add duplicate tests merely to increase counts; it should close reporting,
traceability, distribution, comparison, and CI-execution gaps.

## Recommended MVP

1. Add a shared bench-only metadata/statistics module with deterministic unit
   tests through an integration-test include.
2. Migrate callback and streaming-resampler benches from best-of-N to trial
   distributions, JSON reports, explicit work validation, utilization, and
   optional compatible-baseline comparison.
3. Add environment and `RenderedOutput` timing/tail metadata to the existing
   quality JSON without changing its gate/report/skipped policy.
4. Add an Ubuntu CI quick job that enforces deterministic quality gates,
   validates performance reports, and uploads all three JSON artifacts.
5. Leave full migration of FIR/convolver/listening benches to the P1 tasks that
   will change those algorithms, using the shared module established here.

## Compatibility and failure policy

* Revision may differ between baseline and candidate by definition.
* Profile, features, target, OS/architecture, CPU, benchmark mode, and case
  conditions must match before a timing percentage is meaningful.
* Missing git/rustc/CPU metadata is represented explicitly; caller-provided CI
  revision metadata may override local discovery.
* Corrupt JSON, duplicate/missing cases, non-finite timing, or incompatible
  reports fail an explicitly requested comparison with a named diagnostic.
* Without `--baseline`, performance timing stays report-only. Shared CI runners
  enforce workload/report validity, not an absolute nanosecond budget.
