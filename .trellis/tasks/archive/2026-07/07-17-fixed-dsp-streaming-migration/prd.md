# 迁移固定 DSP 与实时回调链

## Goal

将固定 1:1 processors、8 个 adapters、`DspChain` 与 callback builder 迁移到统一 streaming trait，并完成旧公共 API 的直接切换。

## Parent

* `../07-16-audio-core-quality-correctness/prd.md`

## Requirements

* 迁移 EQ、saturation、crossfeed、limiter、volume、noise shaper、dynamic loudness 与 convolver adapters。
* `DspChain` 存储新的 trait object，并让固定 stages 保持原地、零额外复制的 callback 路径。
* 迁移 output-chain callback builder、tests、examples、benches 与公开 re-export。
* 移除 `AudioProcessor` / `ProcessResult`，不提供 deprecated 或 feature-gated 兼容层。
* README、rustdoc 与 CHANGELOG 提供 breaking-change 迁移示例。

## Acceptance Criteria

* [x] 除迁移说明外，仓库不再引用旧 trait/result。
* [x] 全部固定 adapters 正确报告 consumed=produced，bypass 在 in-place/out-of-place 下等价。
* [x] Callback 预热后无分配/锁/log/I/O/panic。
* [x] 当前功能测试迁移并通过；all-features/no-default-features 均编译。
* [x] 512-frame callback median 回退不超过父 PRD 约定，或有明确批准。

## Dependencies

* `07-17-streaming-trait-contract`
