# Mirrored Saturation FIR History Design

## Live state contract

`OversamplingChannelState::filter_index` currently points to the next circular
write position. `evaluate` starts at the newest residual (`index - 1` with
wrap), then multiplies coefficients in declaration order while walking history
newest to oldest. The wrap decision occurs for every one of the 17 or 33 taps.

Quality changes do not reinterpret a live ring at another tap count. They call
`prepare_nonlinear_state_from_history`, reset oversampling state, and rebuild it
with the newly selected ratio/filter. High-pass topology changes and sample-rate
changes also reset the state. This permits a mirrored layout whose active
window is defined by the current tap count.

## Retained layout

Use `2 * OVERSAMPLING_MAX_FILTER_TAPS` fixed storage. Make the state index point
to the newest residual. On each push, decrement the index modulo the active tap
count and write the residual at both `index` and `index + tap_count`.

The slice `history[index..index + tap_count]` is then ordered newest to oldest.
Zipping this slice with the coefficient table preserves the old product and
accumulator order while removing the per-tap wrap branch.

## Rejected shortcut

A conventional mirrored ring exposes an oldest-to-newest forward window.
Because both coefficient tables are symmetric, this has the same real-number
FIR response, but it reverses the sequence of f64 additions. It therefore does
not satisfy the bit-for-bit compatibility goal and is not used here.

## Required oracle

Tests need a test-only copy of the old circular push/evaluate behavior. Feed
both states deterministic residual sequences that cross the ring boundary
multiple times, assert every evaluate result with `to_bits`, and repeat after
reset and initialize. Existing end-to-end quality, lifecycle, and benchmark
gates remain required because a state-level oracle alone cannot prove callback
timing or transition behavior.
