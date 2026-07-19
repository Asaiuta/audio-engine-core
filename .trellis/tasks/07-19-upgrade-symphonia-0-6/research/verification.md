# Task Verification Status

Checked on 2026-07-19 with Rust 1.93.1 on the repository's Windows target. The
crate MSRV is declared as Rust 1.87 because existing DSP code uses APIs
stabilized in 1.87; Symphonia 0.6 itself requires 1.85.

- `cargo check --lib` passed with Symphonia 0.6.0.
- `cargo test --lib` passed: 347 tests.
- `cargo test --lib --no-default-features` passed: 339 tests.
- `cargo fmt --all -- --check` passed.
- `cargo clippy --all-targets -- -D warnings` passed.
- `cargo clippy --all-targets --no-default-features -- -D warnings` passed.
- `cargo doc --no-deps` and `cargo doc --no-deps --all-features` passed.
- `cargo check --no-default-features --features http` and
  `cargo check --no-default-features --features loudness-db` passed.
- `cargo package --allow-dirty --offline` passed; the package verification
  compiled successfully.
- `git diff --check` passed.

The existing seven repository benches still explicitly exclude the decoder, so
the decoder comparison uses the task-local external comparator documented in
[`decoder-performance-comparison.md`](decoder-performance-comparison.md). Two
reversed-order release runs used 31 trials per version across WAV, stereo FLAC,
Ogg/Vorbis, and 6-channel FLAC. Every workload had a lower 0.6 median decode
time; the observed two-run median-change range was about -7% to -35% depending
on codec and channel count. WAV/FLAC output was bit-identical; Vorbis differed
only by a maximum `2.98e-8` sample delta and retained the same frame count.

This is evidence for the local borrowed streaming path under the actual 0.6
default SIMD configuration, not an end-to-end playback claim. The benchmark
does not cover MP3/AAC, network sources, cold-disk latency, or `decode_all`
allocation cost. Raw reports are retained as
`decoder-performance-comparison-final1.json` and
`decoder-performance-comparison-final2.json`.
