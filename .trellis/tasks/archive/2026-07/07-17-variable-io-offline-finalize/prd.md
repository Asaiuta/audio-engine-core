# 修复可变 I/O 与离线 Finalize

## Goal

用统一 streaming 生命周期修复 SoXR 输入丢失、错误 flush/reset 与 offline limiter/tail 截断，并实现可组合的延迟补偿和尾部保留策略。

## Parent

* `../07-16-audio-core-quality-correctness/prd.md`

## Requirements

* SoXR 每次调用同时推进 `input_frames` / `output_frames`，输出不足时通过 backpressure 继续处理。
* 使用原生 `drain()` 直到 0；reset 调用 native `clear()` 并清除所有 Rust state。
* Offline chain 将上游 finalize 输出继续送过全部下游 stage，再排空下游自身 tail。
* 默认 render 补偿累计 algorithmic latency、保留 semantic tail；另提供 raw causal policy。
* 未知 tail 在 dither 前使用可配置能量阈值、静音保持时长和最大尾长；达到上限显式返回 `tail_truncated`。

## Acceptance Criteria

* [x] 48→96 kHz 的短/长输入和随机分块均不丢帧；输出长度符合明确舍入契约。
* [x] Drain 终止且幂等，reset 后零输入不泄漏旧音频。
* [x] 末帧 impulse 不消失，默认模式首 impulse 时间对齐，raw/default 内容可互相验证。
* [x] 有限 convolution/limiter/resampler tail 正确传播；未知 tail 终止不依赖 block size。
* [x] Streaming 热路径使用预分配 scratch 并通过 no-allocation 检查。

## Dependencies

* `07-17-streaming-trait-contract`
* `07-17-fixed-dsp-streaming-migration`
