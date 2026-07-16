# 统一 Streaming Trait 生命周期契约

## Goal

建立对象安全、零分配友好且能表达可变 I/O 的统一音频流生命周期，为后续 processor、chain、resampler 与 offline renderer 迁移提供唯一契约。

## Parent

* `../07-16-audio-core-quality-correctness/prd.md`

## Requirements

* 引入零拷贝 interleaved `f64` `AudioBlockRef` / `AudioBlockMut`，验证 channels、完整帧和容量。
* 定义安全的 in-place/out-of-place buffer 表达、consumed/produced progress、backpressure、finished 与 typed error。
* 定义 algorithmic latency、semantic tail、幂等 finish/drain 和彻底 reset。
* Trait 保持 object-safe、`Send`，热路径不分配、不锁、不记录日志、不 panic。
* 为固定 1:1、部分消费、输出不足、零进度、有限/未知 tail 编写契约测试。
* 本子任务可在内部迁移期与旧 trait 同时存在，但不得形成发布兼容层；旧 API 在后续迁移子任务中直接移除。

## Acceptance Criteria

* [ ] 合法 block view 零拷贝；零 channels、残缺帧和无效容量返回明确错误。
* [ ] In-place 与 out-of-place 共享相同进度与生命周期语义。
* [ ] `NeedOutput` 可恢复；非法连续零进度被检测，finish 最终稳定返回 finished/0。
* [ ] latency/tail 单位和跨采样率换算契约有文档与测试。
* [ ] 核心契约测试和 rustdoc 通过，新增热路径通过 no-allocation 检查。

## Out of Scope

* 迁移全部 adapters/chain。
* 泛化 `f32`、整数 sample type 或 planar layout。
* 修复具体 DSP 数学缺陷。

## Dependencies

* 无；这是其他子任务的基础。

## Verification

* `cargo fmt --all -- --check` passed.
* `cargo clippy --all-targets --all-features -- -D warnings` passed.
* `cargo clippy --all-targets --no-default-features -- -D warnings` passed.
* `cargo test --all-features` passed: 240 tests.
* `cargo test --no-default-features` passed: 232 tests.
* `cargo rustdoc --all-features -- -D warnings` passed.
* Contract-focused tests passed: 13 tests covering zero-copy views, mode-specific progress, rate-tagged timing, stateful finish/reset, object safety, backend diagnostics, and no-allocation in-place/out-of-place/finish paths.
* Code review found no critical issues. Four important contract gaps (partial in-place progress, process/finish state confusion, cross-rate timing metadata, backend error preservation) were fixed before this verification.
* `task.py validate` passed with inline-mode empty context files.
