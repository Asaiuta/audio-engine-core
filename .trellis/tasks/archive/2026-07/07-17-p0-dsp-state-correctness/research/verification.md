# P0 DSP State Correctness Verification

## Regression-first evidence

The new regression tests were first run against the pre-fix production
implementation. They failed for the intended reasons:

* EQ transition completion left the active branch's `z1` at zero while the
  independently processed target branch had non-zero delay state. Copying only
  coefficients therefore failed complete active/target state equality.
* Constructing `LoudnessNormalizer` with `enabled=false` still published
  `enabled=true`, and the configured normalization mode remained the atomic
  default rather than the caller's value.
* The old low/high-shelf coefficients differed from the independently written
  RBJ reference by more than the `1e-12` coefficient tolerance because the
  shelf term multiplied by `sin(w0)` twice.
* A direct dynamic-loudness rate change erased smoother progress (a
  representative band changed from approximately
  `(current=0.26953, target=2.664, remaining=usize::MAX)` to zeroed state), and
  the adapter rebuilt strength from the published `0.37` value back to `1.0`.

After the production fixes, the focused suites passed: seven EQ tests, two
normalizer tests, 21 dynamic-loudness tests, and the adapter rate-change state
test. Coverage includes tone and impulse transitions, irregular stereo chunks,
all five normalization modes, RBJ coefficient and analytical-response oracles,
sample-rate state preservation/reset boundaries, and EQ transition
no-allocation.

## Project gates

| Command | Result |
| --- | --- |
| `cargo fmt --all -- --check` | pass |
| `git diff --check` | pass |
| `cargo test --all-features` | pass: 257 unit tests + 2 doc tests |
| `cargo test --no-default-features` | pass: 249 unit tests + 2 doc tests |
| `cargo clippy --all-targets --all-features -- -D warnings` | pass |
| `cargo clippy --all-targets --no-default-features -- -D warnings` | pass |
| `cargo build` | pass |
| `cargo build --no-default-features --features http` | pass |
| `cargo build --no-default-features --features loudness-db` | pass |
| `cargo doc --no-deps` | pass |
| `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features` | pass |
| `cargo package --allow-dirty --offline` | pass: 205 files, independent verification build |

## Objective audio-quality gate

Command:

```text
cargo bench --bench audio_quality_measurements -- --quick --enforce \
  --out .trellis/tasks/07-17-p0-dsp-state-correctness/research/audio-quality-after.json
```

The enforced run passed. The corrected dynamic-loudness shelf measured
`+8.412588920844339 dB` at 40 Hz (gate: at least `+6 dB`) and `+2.83 dB` at
3 kHz. Other relevant guardrails remained green, including 10-band EQ target
error `0.0000 dB`, crossfeed continuity delta `0.000e0`, limiter true-peak
output `-1.00 dBTP`, and saturation alias reduction `+16.56 dB`.

The optional EBU loudness and true-peak corpora remained explicitly skipped
because 53 and 9 reference files, respectively, are not bundled. The full
output-chain `-0.610 dBTP` result remains report-only, as documented; it is not
represented as a conformance pass.

## Callback performance comparison

`audio_callback_chain_perf --quick` was run five times for both detached
baseline `bb08e45` and the candidate on the same machine. At 512 frames:

| Scenario | Baseline median | Candidate median | Delta | Candidate buffer time |
| --- | ---: | ---: | ---: | ---: |
| Active DSP, no convolver | 115.921 ns/sample | 120.542 ns/sample | +3.99% | 123.435 us |
| Active DSP, with convolver | 126.019 ns/sample | 128.660 ns/sample | +2.10% | 131.747 us |

The quick-run ranges overlap (`114.191-134.723 ns/sample` for the candidate
without convolver, with similarly visible scheduling variance in both runs).
The changed production code adds no steady-state per-sample allocation, lock,
I/O, logging, or new loop. Even the slowest observed candidate buffer
(`146.493 us`) used about 1.4% of the approximately 10.7 ms callback period at
48 kHz, so the measurement does not show a material callback-budget regression.
