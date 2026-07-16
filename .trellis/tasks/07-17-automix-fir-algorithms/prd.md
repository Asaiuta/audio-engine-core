# 修复 AutoMix 与 FIR EQ 算法

## Goal

修复 AutoMix 时间基准/key 输出与 FIR EQ 退化配置、统一增益和 minimum-phase window 的 P1 算法错误。

## Parent

* `../07-16-audio-core-quality-correctness/prd.md`

## Requirements

* BPM 换算使用实际 hop/sample rate，不再固定假设 50 Hz。
* 用已知 BPM fixtures 验证 tempo；key 字段要么实现并验证，要么从公开能力中明确移除/标记 unsupported，不能恒为 `None` 却暗示可用。
* FIR 1-tap 输出 finite 并具有明确纯增益/单位 impulse 语义。
* 全频统一增益不得被无条件归一化回 unity。
* 修正 minimum-phase tail window 方向，并以频响/能量时序 oracle 验证。

## Acceptance Criteria

* [ ] 多个采样率/hop 下已知 BPM 估计落入明确容差。
* [ ] Key 能力的公开契约与实际实现一致。
* [ ] 1-tap、统一 ±gain、minimum/linear phase 均 finite 且命中参考指标。
* [ ] FIR 生成与 apply 基准无未经批准的显著回退。

## Dependencies

* `07-17-audio-quality-performance-gates`

