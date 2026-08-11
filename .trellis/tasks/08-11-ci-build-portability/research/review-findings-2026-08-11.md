# Review Findings 2026-08-11 — CI / Build / Portability

Source: engineering-quality deep-review agent report from the 2026-08-11
six-track review (problem table rows 4-13; rows 1-3 — packaging leak and two
README claims — were fixed in 1.0.1).

| # | Location | Severity | Finding |
|---|----------|----------|---------|
| 4 | `benches/audio_lockfree_params_perf.rs:430-443` | med-low | "7 ns vs ~50 ns" comparison: legacy split-atomic baseline reads each of ~30 fields through `#[inline(never)]` + `black_box` — a real naive implementation inlines, so a large share of the ~50 ns is constructed call overhead. 7 ns and 83 ns (ArcSwap guard) are fair. Bench is also single-trial with no warmup/median, below the repo's callback-bench statistical standard. |
| 5 | ci.yml vs CONTRIBUTING:57-62 | med-low | CONTRIBUTING requires `rubato,http` and `rubato,loudness-db` verification; CI tests only the all-features and rubato-only endpoints — no cargo-hack-style combination coverage. |
| 6 | `build.rs:83`, `:195-198` | low | `/MACHINE:X64` and `HostX64/x64` hardcoded; Windows ARM64/x86 MSVC targets fail in the MSYS2 import-lib path. |
| 7 | `build/windows_runtime.rs:24-50` | low | Build script writes outside OUT_DIR (`target/{profile}`, `deps`, `examples`) against the Cargo contract; `fs::copy` non-atomic under concurrent builds / locked DLLs. Self-aware pragmatism; `copy_if_changed` mitigates the common path. |
| 8 | `build.rs:122-138` | low | Personal-machine `%USERPROFILE%\scoop\apps\msys2` probe ships in the release build script. |
| 9 | `.gitignore:4-6` | low | Comment claims the lockfile "is tracked at the workspace root" — Cargo.lock is not tracked; with rust-cache, CI dependency drift is invisible. Lock audit: all crates.io registry sources, no git/path deps, no suspicious pins. |
| 10 | ci.yml:161-179 vs `docs/installation.md:22-29` | low | Docs present vcpkg as the preferred Windows install path; CI provisions only MSYS2/pkg-config — the `vcpkg::find_package` branch (`build.rs:30`) has zero CI coverage. |
| 11 | ci.yml (global) | low | No `schedule:` cron, no cargo-audit/cargo-deny; pinned-nightly disappearance or dependency advisories surface only on the next push. clippy/doc ride floating stable (common tradeoff, noted). |
| 12 | Cargo.toml:20, :38 | low/design | `symphonia features=["all"]` — downstream cannot trim codecs; default features include rusqlite bundled (full SQLite C build) + reqwest/rustls — heavy default for a "core" crate; `soxr`+`rubato` co-enabled silently prefers soxr (documented; `RESAMPLER_BACKEND_NAME` observable). |
| 13 | `tests/semver-baseline/` (5.2M + 3.7M JSON) | low/known | rustdoc JSON baselines tracked in-repo and growing per release; CONTRIBUTING already notes a future registry-baseline switch. |

## Positive context to preserve (from the same report)

3 OS × 2 feature matrices with a genuinely bare pure-rust runner; MSRV read
dynamically from Cargo.toml; three-valued pinned-nightly probing
(`tests/public_api.rs:75-119` — "a gate that reports green because it could
not run is worse than no gate"); dual API freeze (6.4k-line text snapshot +
cargo-semver-checks `--release-type patch` vs committed baselines); README
compiled as doctests; bench gates uploading JSON artifacts with
`if: always()`; the deliberate refusal to assert absolute timings on shared
runners (ci.yml:247-249).
