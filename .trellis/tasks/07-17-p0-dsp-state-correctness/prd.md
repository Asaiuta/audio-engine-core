# 修复 P0 DSP 状态与数学正确性

## Goal

修复 EQ 状态交接、loudness 配置传播与 dynamic-loudness shelf/builder 的发布阻断级错误。

## Parent

* `../07-16-audio-core-quality-correctness/prd.md`

## What I Already Know

* `Equalizer` runs current and target biquads throughout the 1,024-frame
  transition, but completion calls `copy_coefficients_from` and intentionally
  keeps the current branch's stale `z1/z2` instead of adopting the target
  branch state.
* `LoudnessNormalizer::new` seeds `AtomicLoudnessState` with its defaults, and
  `set_config` currently republishes neither `enabled` nor `mode`.
* Dynamic-loudness shelf code already computes `2 * sqrt(A) * alpha`; multiplying
  that value by `sin(w0)` again inside each coefficient is the duplicated factor.
  Existing `legacy_*_shelf_coeffs` tests reproduce the same defect and are not
  an independent correctness oracle.
* `DynamicLoudnessProcessor::set_sample_rate` replaces the complete processor
  with `DynamicLoudness::new`, discarding the current volume factor and strength.
  The direct processor can instead rebuild rate-dependent geometry in place.

## Requirements

* EQ crossfade 完成后采用目标滤波器的系数和完整内部状态。
* `LoudnessConfig.enabled`、`mode` 及相关字段完整传播到 atomic snapshot 和 processor。
* Dynamic loudness shelf 严格对照 RBJ/W3C 公式，移除重复 `sin(w0)` 因子。
* Sample-rate 更新不得通过重建静默丢失 volume、strength、smoother 目标或其他用户状态；允许并记录采样率相关 biquad history reset。
* 每项缺陷先落旧实现失败的最小回归测试。
* 修复不得在 callback process 路径引入分配、锁、日志、I/O 或 panic。

## Acceptance Criteria

* [x] EQ transition 相对持续目标 reference 的边界最大线性误差 `<=1e-9`。
* [x] `enabled=false, Album` 等配置能精确往返并控制实际处理。
* [x] 所有五种 normalization mode 均能从 config 精确映射到 atomic state；`set_config`、`set_enabled`、`set_mode` 后配置与实际处理一致。
* [x] Low/high shelf 在代表性采样率、增益与截止频率下，系数和解析频响与独立 W3C/RBJ reference 在 `1e-12`/`1e-9 dB` 容差内一致。
* [x] Sample-rate 变更前后 volume factor、strength、smoother current/target 不变；biquad history 被明确重置并在新采样率重建正确系数。
* [x] Mono/stereo、chunking、reset 与 no-allocation 测试通过。

## Definition of Done

* 旧实现会失败的 EQ、loudness config、RBJ shelf 和 sample-rate state 回归测试全部通过。
* `cargo fmt --all -- --check`、双 feature 测试与严格 Clippy 通过。
* 相关 callback/no-allocation 与 objective quality 指标无回退。
* README/CHANGELOG/spec 在公开行为或维护契约变化时同步。

## Technical Approach

* EQ target branch remains the authoritative post-transition branch. On the
  terminal transition frame, clone the complete target biquad (coefficients and
  delay state) into the active bank; no heap allocation is required.
* Centralize normalization-mode encoding, publish config `enabled` and `mode`
  during construction and `set_config`, and keep explicit setters synchronized
  with the stored config.
* Express the shelf intermediate as `two_sqrt_a_alpha = 2 * sqrt(A) * alpha`
  and use it directly in the RBJ coefficient equations. Replace the legacy
  self-reference with a separately written W3C/RBJ oracle plus transfer-function
  response checks.
* Change the dynamic-loudness adapter to call the processor's in-place
  sample-rate update. Preserve smoother current/target and user control state,
  recompute the smoother coefficient, reset incompatible biquad histories, and
  rebuild coefficients at the preserved current gains.

## Decision (ADR-lite)

**Context**: These failures all come from treating control geometry and DSP
history as interchangeable. Copying only coefficients loses the state of the
branch actually heard during a crossfade; rebuilding a processor for a sample
rate change loses control state that is independent of sample rate.

**Decision**: State ownership follows the signal branch: adopt the full target
state after EQ crossfade, while dynamic-loudness sample-rate updates preserve
control/smoother state and reset only rate-dependent filter history. Config is
published explicitly rather than relying on atomic defaults.

**Consequences**: Transition and configuration behavior becomes referenceable
and deterministic without adding callback allocation. A sample-rate change may
still reset biquad delay history because old-rate state has no valid direct
mapping into the new geometry; that reset is explicit and tested.

## Out of Scope

* Redesigning EQ crossfade topology or defining a new policy for repeated gain
  changes before an existing 1,024-frame transition completes.
* P1 AutoMix/FIR, saturation, noise-shaper, crossfeed, or convolver work.
* Replacing independent loudness atomics with a new transactional snapshot
  architecture.
* Attempting to transform old-rate biquad delay state into a new sample-rate
  domain instead of resetting it.

## Technical Notes

* Primary files: `src/processor/eq.rs`,
  `src/processor/loudness/{normalizer,atomic_state}.rs`,
  `src/processor/dynamic_loudness.rs`, and `src/processor/adapters.rs`.
* The task is a bounded P0 correctness batch. Public APIs should only change
  where needed to keep stored config and runtime state consistent.

## Research References

* [`research/current-defect-contracts.md`](research/current-defect-contracts.md)
  — current source mechanisms, chosen state ownership, oracle design, and test
  matrix.
* [`research/verification.md`](research/verification.md)
  — regression-first failure evidence, final quality gates, objective audio
  measurements, and callback performance comparison.
* [`../07-16-audio-core-quality-correctness/research/audio-quality-gates.md`](../07-16-audio-core-quality-correctness/research/audio-quality-gates.md)
  — W3C/RBJ reference source and the parent task's Layer 1/2 evidence policy.

## Dependencies

* `07-17-fixed-dsp-streaming-migration`
