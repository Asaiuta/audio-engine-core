# 修复 P0 DSP 状态与数学正确性

## Goal

修复 EQ 状态交接、loudness 配置传播与 dynamic-loudness shelf/builder 的发布阻断级错误。

## Parent

* `../07-16-audio-core-quality-correctness/prd.md`

## Requirements

* EQ crossfade 完成后采用目标滤波器的系数和完整内部状态。
* `LoudnessConfig.enabled`、`mode` 及相关字段完整传播到 atomic snapshot 和 processor。
* Dynamic loudness shelf 严格对照 RBJ/W3C 公式，移除重复 `sin(w0)` 因子。
* Sample-rate 更新不得通过重建静默丢失 volume、strength 或其他用户状态。
* 每项缺陷先落旧实现失败的最小回归测试。

## Acceptance Criteria

* [ ] EQ transition 相对持续目标 reference 的边界最大线性误差 `<=1e-9`。
* [ ] `enabled=false, Album` 等配置能精确往返并控制实际处理。
* [ ] Shelf 代表性频响与解析 reference 在约定容差内一致。
* [ ] Sample-rate 变更前后用户参数不变，状态变化符合文档。
* [ ] Mono/stereo、chunking、reset 与 no-allocation 测试通过。

## Dependencies

* `07-17-fixed-dsp-streaming-migration`

