# SoXR Interleaved Stereo v2 Validation

Date: 2026-07-27

## Production change

Exactly two channels now use one native `Soxr<Stereo<f64>>` stream over
validated caller-owned interleaved buffers. Other channel counts retain the
existing independent-mono fallback. The benchmark-only raw libsoxr adapter and
native comparison shims remain under `benches/`; none participates in
production backend selection.

The production comparison algorithm ID is
`audio_engine_core_soxr_interleaved_stereo_high_linear_v2`.

## Direct effect versus the former per-channel route

The quick reports `soxr-stereo-candidate-quick.json` (former per-channel v1)
and `soxr-stereo-aligned-capacity-quick.json` (interleaved v2) use the same
machine, compiler, format, rates, quality recipe, 512-frame schedule, exact
work policy, and common output capacity. Negative deltas are improvements.

| Direction | Steady delta | Setup delta | Reset delta | Drain delta |
| --- | ---: | ---: | ---: | ---: |
| 44.1 -> 48 kHz | -56.16% | -44.45% | -47.73% | -58.28% |
| 48 -> 44.1 kHz | -39.42% | -33.68% | -44.30% | -49.69% |

The v2 output is bit-identical to independent mono references, uses one native
backend, owns zero adapter PCM scratch, and passes arbitrary-chunk,
reset/fresh, terminal-drain, exact-duration, and no-allocation tests.

## Strict v2 versus raw stereo libsoxr

All strict rows use interleaved f64, the same libsoxr HQ/Bits20 linear recipe,
one native stereo stream, 512-frame callers, common per-rate output capacity,
exact complete work, and pinned logical core 2. A result within 2% is classified
as tied; no sub-2% row is described as a win.

| Evidence | Direction | Steady delta | Setup delta | Reset delta | Drain delta | Classification |
| --- | --- | ---: | ---: | ---: | ---: | --- |
| quick | 44.1 -> 48 | +0.97% | -0.13% | -0.85% | -4.31% | tied |
| quick | 48 -> 44.1 | -2.60% | -3.94% | -7.36% | -8.23% | faster |
| heavy A1 | 44.1 -> 48 | +1.54% | -1.66% | +1.91% | +2.00% | tied |
| heavy A1 | 48 -> 44.1 | +11.11% | -4.14% | -2.43% | -0.09% | failed outlier |
| heavy A2 | 44.1 -> 48 | +0.34% | -4.59% | -0.38% | -2.21% | tied |
| heavy A2 | 48 -> 44.1 | +1.04% | -3.47% | +2.47% | +2.55% | tied |
| heavy A3 | 44.1 -> 48 | +1.88% | -2.17% | +2.32% | +0.93% | tied |
| heavy A3 | 48 -> 44.1 | -1.94% | -5.63% | -1.65% | -2.80% | tied |

Heavy A1's reverse steady row conflicts with quick, A2, and A3 while work and
quality remain identical. It is retained rather than deleted. The host uses
the balanced Windows power plan; pinning controls scheduler placement but not
frequency changes. Two later 15-trial heavy confirmations reproduced the tie
and kept every reset/drain regression below 5%, so the acceptance decision is
statistical parity in both directions, not universal superiority.

Reports:

* `soxr-stereo-aligned-capacity-quick.json`
* `soxr-stereo-v2-pinned-heavy-final.json` (heavy A1)
* `soxr-stereo-v2-pinned-heavy-a2.json`
* `soxr-stereo-v2-pinned-heavy-a3.json`

Every report passed exact work and quality validity. The project and raw rows
have equal complete lengths, impulse positions, tone gains, THD+N, and alias
results for each direction.

## Final-source heavy reruns

Two additional pinned 15-trial heavy reports were generated after the final
format/lint/test pass:

* `soxr-stereo-v2-pinned-heavy-final-source.json`
* `soxr-stereo-v2-pinned-heavy-final-source-a2.json`

The first measured steady deltas of +1.73% forward (tied) and -2.11% reverse,
but retained a +6.40% reverse drain outlier. The second measured +4.27%
forward (a failed single-run steady row) and -2.79% reverse; all reset/drain
rows were then inside 5%. Repeating until a preferred result appeared would be
selection bias, so both reports are retained and no further rerun was used for
acceptance.

Across all five heavy reports (A1, A2, A3, final-source, final-source A2), the
median per-run deltas are:

| Direction | Steady | Setup | Reset | Drain |
| --- | ---: | ---: | ---: | ---: |
| 44.1 -> 48 kHz | +1.73% | -2.17% | +0.00% | +0.70% |
| 48 -> 44.1 kHz | -1.94% | -3.48% | -1.65% | -0.09% |

Every one of the 150 project and 150 raw steady trials has valid exact work and
quality. The acceptance conclusion remains statistical parity in both
directions, with the failed single-run rows visible rather than erased.
