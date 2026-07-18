# Convolver Activation Transition Expansion

## Current behavior

`ConvolverProcessor::sync_convolver` adopts a published kernel or drops to a
dry fixed-1:1 bypass at a block boundary. A replaced kernel is retired through
the bounded ownership mailbox, but there is no audio-domain crossfade. A rate
change with no matching kernel therefore has a safe dry interval, while later
adoption can change the output abruptly when the IR has non-zero response.

Both engines expose a zero-algorithmic-latency head path. The partitioned tail
appears at its natural IR offset; it is not a block delay applied to the whole
wet signal. Dry input and convolution output therefore share one source-frame
timeline for an activation fade. Public metadata that describes partition-size
latency must be corrected separately.

## Feasible patterns

### Hard block-boundary commit

Keep the current behavior and document that kernel publication/enable changes
are discontinuous at a block boundary. It has the lowest callback CPU and no
extra FFT state, but can click on an arbitrary waveform or when a long IR is
introduced.

### Dry/wet transition for activation (bounded, recommended if in scope)

When moving from dry waiting to a matching kernel, keep the dry input and
convolution output on the same block timeline and apply a complementary
sample-rate-derived smoothstep of `ceil(0.005 * active_sample_rate_hz)` frames
(5 ms, verified by transient oracles and listening fixtures). The first active block needs one convolution
state and one delay/copy path, not two kernels. The same transition can fade to
dry when disabling. It adds a small bounded multiply/add cost only during the
transition and preserves the existing single-kernel ownership model.

A sample-rate switch is a hard correctness boundary: once the new rate becomes
active, the old-rate kernel cannot continue merely to complete a fade. Without
advance coordination, the old effect may disappear at that boundary. The
clickless guarantee applies to same-rate disable/enable and to a later matching
kernel fading in from the new-rate dry waiting state.

### Dual-kernel transition for replacement

For a live IR replacement, run old and new kernels concurrently and crossfade
their aligned outputs over a bounded window. This is the most robust sonic
behavior, but temporarily doubles FFT/convolution CPU and requires a second
preallocated kernel state or a control-side rendered transition. It is not
appropriate for the primary MVP unless measured publication churn is common.

## Scope implication

Activation from a rate-mismatch dry wait can be made clickless with bounded
extra work and no new ownership queue. General dual-kernel replacement is a
separate performance-heavy feature. A generic event transport for every DSP
stage is also separable from the selected Saturation event slice and should not
be implied by adding activation smoothing.
