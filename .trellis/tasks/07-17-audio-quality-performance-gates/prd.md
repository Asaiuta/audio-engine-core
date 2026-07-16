# 建立音质与实时性能门禁

## Goal

把本轮正确性、客观音质与实时性能结论固化为持续可执行、可分类并可复现的门禁。

## Parent

* `../07-16-audio-core-quality-correctness/prd.md`

## Requirements

* 增加 consumed/produced、随机分块、finish/reset、末帧 impulse、latency/tail 的性质测试。
* Quality report 保持 gate/report/skipped，增加完整输出 length/tail/true-peak 与修复后算法指标。
* 缺失 EBU corpus 时显式 skipped；发布 conformance 结论前必须在受控环境跑外部 vectors。
* Callback/resampler benches 输出环境元数据、median、p95、deadline utilization 与机器可读报告。
* 零稳态分配为硬门禁；同环境 median 回退超过 10% 必须修复或明确批准。

## Acceptance Criteria

* [ ] 每个父任务 P0 probe 都有旧实现失败、修复后通过的测试。
* [ ] `--enforce` 的失败信息包含 metric、measured 与 threshold。
* [ ] Quality/performance 报告可追溯到 revision、features 与运行条件。
* [ ] EBU 缺失不被计为通过，完整输出 true-peak 的 gate/report 状态诚实可见。
* [ ] 快速门禁适合本地/CI，完整基准有可复现命令与结果记录。

## Dependencies

* 前四个 P0/streaming 子任务。

