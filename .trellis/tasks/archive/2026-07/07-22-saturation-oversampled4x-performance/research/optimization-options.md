# Oversampled4x Optimization Options

## Current hotspot

The live implementation already computes nonlinear residuals and evaluates the
decimation FIR once per source frame. The per-source-frame 4x work is still
four interpolations and waveshaper calls plus a 33-tap circular dot product.
The dot-product loop branches on every tap to wrap the ring index, while the
ratio and phase loop remain runtime-shaped even when a buffer uses one quality.

## Options

### Fixed-ratio dispatch

Select `Oversampled2x` or `Oversampled4x` once before entering the frame loop,
then compile a const-ratio kernel. This removes repeated ratio-dependent loop
control and enables unrolling/constant propagation without changing the public
algorithm.

### Mirrored FIR history

Store each pushed residual at both `index` and `index + taps` in a fixed
preallocated array. The newest `taps` samples then form one contiguous window;
the symmetric coefficient tables allow the dot product to use the chronological
window without changing the mathematical FIR response. This removes wrap
branches and makes compiler vectorization possible. The state remains bounded
and setup-only.

### Explicit SIMD

An AVX2/FMA implementation could process four or eight f64 products at once,
but it introduces target-specific unsafe code or a new portable-SIMD dependency.
It should only be considered after the portable structural changes are measured.

### Sparse residual activity

Below-threshold linear interpolation cannot cross the threshold when both
endpoints are within the threshold interval, so a proven quiet region can avoid
waveshaper calls. However, FIR history still needs zero advancement and the
activity flag must preserve pending tails and chunk-independent output. This is
a separate follow-up, not part of the first kernel change.

## Chosen order

1. Fixed-ratio dispatch and const four-phase kernel.
2. Mirrored contiguous history and dot-product measurement.
3. Only if needed, investigate SIMD or sparse activity with independent quality
   and lifecycle evidence.
