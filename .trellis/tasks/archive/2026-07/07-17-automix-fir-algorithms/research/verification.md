# Verification Evidence

Date: 2026-07-17 (Asia/Shanghai)

## Defect-mechanism regressions

The new tests were run against the old behavior before the fixes:

* A 120 BPM spectral-flux fixture at 44.1 kHz finalized as
  `69.76744186046511` BPM because the old code passed 50 Hz instead of
  `44_100 / 512`.
* The multi-rate fixture folded a 180 BPM 50 Hz impulse train to 60 BPM under
  the old fixed-lag/peak policy.
* Serialized AutoMix output reported schema version 1 and had no explicit key
  capability status.
* Linear one-tap FIR produced non-finite output.
* A uniform linear-phase -6 dB request measured approximately 0 dB because the
  old inverse-1-kHz normalization erased absolute gain.

After the fixes, tempo tests cover 60/120/180 BPM at 50 Hz,
`44_100/512`, and `48_000/512` within 2% relative error. Invalid rates, short
input, flat input, schema v2 `key_status = "unsupported"`, one-tap reference
gain, uniform +/-6 dB, representative-band DFT response, minimum-phase taper
direction, and energy-centroid ordering all pass.

## FIR performance report and baseline gate

Commands:

```text
cargo bench --bench audio_fir_eq_perf -- --quick --enforce \
  --out target/bench-reports/fir-eq-prebuilt-baseline.json

cargo bench --bench audio_fir_eq_perf -- --quick --enforce \
  --baseline target/bench-reports/fir-eq-prebuilt-baseline.json \
  --out target/bench-reports/fir-eq-prebuilt-candidate.json
```

The final confirmation report deserialized with schema version 1, probe
`audio_fir_eq_perf`, quick mode, 9 unique cases, 9 comparisons, and every
comparison passing the default 10% median gate. It retained seven raw trials
per distribution, complete environment/conditions, finite IR/output work, and
overlap-save apply routing.

Representative final medians on this recorded Windows/rustc/CPU environment:

| Case | Median |
| --- | ---: |
| linear regeneration, 511 taps | 33.393 us/regeneration |
| linear regeneration, 1023 taps | 67.547 us/regeneration |
| linear regeneration, 2047 taps | 143.729 us/regeneration |
| minimum regeneration, 511 taps | 105.165 us/regeneration |
| minimum regeneration, 1023 taps | 243.883 us/regeneration |
| minimum regeneration, 2047 taps | 535.558 us/regeneration |
| linear apply, 511 taps, stereo/512 frames | 14.385 ns/sample |
| linear apply, 1023 taps, stereo/512 frames | 26.692 ns/sample |
| linear apply, 2047 taps, stereo/512 frames | 65.438 ns/sample |

All nine medians in the accepted prebuilt comparison were non-regressions; the
closest to baseline was -3.402%. Absolute numbers remain machine/load-specific
and are not a universal performance claim.

Earlier immediate comparisons correctly failed the 10% gate despite no code
change because local runs moved by up to 45% with compilation heat,
scheduling, load, and CPU-frequency state. Quick timing windows were increased
from 200/400 to 1,000/2,000 regeneration/apply operations per trial, and the
accepted pair was generated only after the release binary was already built.
The threshold was not relaxed. The raw distributions still show that this
laptop is noisy, so the numbers must not substitute for a controlled benchmark
host; the failure behavior is retained as evidence that `--enforce` rejects a
measured regression rather than merely writing JSON.

## Final project matrix

All commands passed on the final source state:

```text
cargo test --all-features
  266 unit tests + 8 benchmark-support tests + 2 doctests passed

cargo test --no-default-features
  258 unit tests + 8 benchmark-support tests + 2 doctests passed

cargo clippy --all-targets --all-features -- -D warnings
cargo clippy --all-targets --no-default-features -- -D warnings
cargo fmt --all -- --check

DOCS_RS=1 RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --all-features
cargo package --allow-dirty --all-features --offline
  212 files packaged; package verification build passed
```

The first package attempt inside the restricted environment could not update
the crates.io index because Windows credentials/network were unavailable. An
approved network retry populated the index and passed; the final source state
then passed the offline package command above.

## Review and limitations

* `trellis-check` review found no remaining Critical or Important issue. It did
  identify that the first harmonic selection accepted any point on a broad
  autocorrelation shoulder; selection now requires the shortest qualifying
  local peak, and the full matrix was rerun.
* No new dependency, callback allocation, lock, I/O, logging, or convolver
  routing change was introduced. AutoMix and FIR generation remain offline or
  control-thread work.
* Musical-key detection remains intentionally unsupported. This task does not
  claim real-track key accuracy, listening-test superiority, or global
  best-in-class tempo/FIR performance.
* GitHub-hosted CI was not executed locally. The workflow now runs the FIR
  quick enforce/report command and uploads its JSON alongside the other three
  artifacts; CI timing stays report-only without a compatible baseline.
