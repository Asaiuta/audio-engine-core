# 建立音质与实时性能门禁

## Goal

把本轮正确性、客观音质与实时性能结论固化为持续可执行、可分类、可比较并可复现的门禁。门禁必须区分确定性正确性失败、报告型音质证据、缺失外部语料和机器相关性能波动，不能把单次 timing 或缺失 corpus 表述为“最佳音质/最高性能”。

## Parent

* `../07-16-audio-core-quality-correctness/prd.md`

## What I Already Know

* 前四个 streaming/P0 子任务已经归档，因此本任务依赖已满足。
* `audio_quality_measurements` 已有 `gate` / `report` / `skipped`、`--enforce` 和 `--out` JSON；失败信息已包含 metric、measured 与 threshold。
* Quality JSON 尚未记录 revision、dirty state、profile、features、rustc、target、OS/CPU 等复现条件；full-output point 只记录 `output_frames`，未发布 `RenderedOutput` 的 latency/tail/truncation metadata。
* `audio_callback_chain_perf` 与 `audio_resampler_streaming_perf` 仍选择 best-of-N，只输出文本，没有 p95、完整 trial samples、环境元数据、JSON 或基线 artifact 比较；`--enforce` 只做 finite/positive 与“产生了输出”检查。
* Convolver 与 lock-free benches 已证明同进程相对门禁比跨机器绝对纳秒阈值稳定；父任务研究建议先演进现有 custom harness，不引入 Criterion/Divan。
* 当前 CI 运行 fmt、双 Clippy、跨平台双 feature tests、docs 和 package，但不执行 quality/performance quick gates，也不保存 machine-readable bench artifacts。
* P0 缺陷已有旧实现失败、修复后通过的回归：resampler consumed/drain/reset、末帧 impulse/finalize、EQ target branch state、loudness config、RBJ shelf 与 sample-rate state preservation。

## Requirements

* 保留现有 custom-main benchmark 架构与 `cargo bench --bench ... -- ...` 入口，不新增统计框架依赖。
* 提供 bench-local 共享报告支持，至少包含 schema version、probe、generated time、mode 与环境信息：revision、dirty state、rustc、target、OS、arch、CPU、Cargo profile、编译 features。
* 环境采集在 git/rustc/CPU 信息不可用时输出明确的 `unknown`/nullable 字段，而不是让 benchmark 失败；允许 CI 通过显式环境变量覆盖 revision。
* `audio_callback_chain_perf` 与 `audio_resampler_streaming_perf` 从 best-of-N 改为 trial distribution，输出 min、median、p95、max、原始 samples、iterations 与 trials。
* Quick/full/heavy 模式保持大致现有总工作量，但 quick 也必须有足够 trial 计算 median/p95，并继续适合本地和 CI。
* Callback report 输出 buffer duration、median/p95 callback time 与 deadline utilization；resampler 明确把 utilization 标为 source-buffer realtime reference，而不是宣称它就是设备 callback 成本。
* 两个性能 bench 支持 `--out <json>`；JSON case key 必须稳定，包含所有场景、API、rate、channels、frames、算法模式与覆盖/排除说明。
* 两个性能 bench 支持 `--baseline <json>` 和默认 10% median 回退阈值；只比较 schema、probe、环境兼容且 case key 完全匹配的报告。
* 传入兼容 baseline 且 candidate median 回退 `>10%` 时，`--enforce` 失败并打印 case、baseline、candidate、regression 与 threshold。无 baseline 时 timing 保持 report-only，`--enforce` 只验证工作量、finite timing 和报告完整性。
* Baseline revision 可以不同；profile、features、target、OS/arch、CPU、模式与 case conditions 不匹配时拒绝伪比较，并给出明确诊断。
* Callback 每个场景在计时外验证输出 finite 且实际执行了预期工作；resampler 继续验证 consumed/produced 与输出帧数，避免以少做工作换取虚假加速。
* Quality JSON 接入相同环境元数据，并把 full-output `rendered_frames`、algorithmic latency、semantic tail 与 `tail_truncated` 暴露到每个 synthetic point。
* 缺失 EBU corpus 时继续显式 `skipped`；CI artifact 和文本摘要必须能看见缺失数量，不能计作 conformance pass。
* CI 增加 Ubuntu quick quality/performance job：quality 使用 `--enforce`，callback/resampler 使用 `--quick --enforce --out` 验证报告生成，并上传三份 JSON artifact。共享 runner 不使用跨 run absolute timing gate。
* 建立 P0 probe → regression test/quality metric 矩阵；已有充分单元/性质测试的缺陷不复制测试，只补缺失的报告或门禁覆盖。

## Acceptance Criteria

* [x] 每个父任务 P0 probe 都映射到旧实现失败、修复后通过的测试或明确 quality metric。
* [x] `audio_quality_measurements --quick --enforce --out ...` 保持 gate/report/skipped 语义，并输出 revision、features、运行条件与 full-output timing/tail metadata。
* [x] Callback/resampler JSON 可反序列化，case key 唯一稳定，所有 timing finite 且 sample 数等于声明 trials。
* [x] Quick/full/heavy 均报告 median、p95、max、deadline/reference utilization 和环境元数据；不再把 best trial 作为代表值。
* [x] 构造的 `+10%` baseline case 通过，`>10%` case 在 `--enforce` 下失败，诊断包含 case、baseline、candidate、measured regression 与 threshold。
* [x] 不兼容 schema/probe/profile/features/target/CPU/case set 的 baseline 被明确拒绝，不生成误导性百分比。
* [x] EBU 缺失不被计为通过；完整输出 true-peak 的 gate/report 状态诚实可见。
* [x] CI quick job 执行三个报告入口并上传 JSON；不对 GitHub shared runner 设置 absolute ns 硬阈值。
* [x] 零稳态分配、双 feature tests、严格 Clippy 和 quality `--enforce` 全部通过。
* [x] README/CONTRIBUTING 记录 quick、full、JSON 与 baseline comparison 的可复现命令和限制。

## Definition of Done

* Tests added for shared statistics, percentile boundaries, environment compatibility, case matching and 10% regression decisions.
* `cargo fmt --all -- --check`、双 feature tests 与严格 Clippy 通过。
* 三个 quick JSON 报告实际生成并检查；quality gate 通过。
* CI、README、CONTRIBUTING、CHANGELOG 与 Trellis spec/研究记录同步。
* 性能结论只引用相同条件的 median/p95 与 utilization，不使用抽象“最高性能”措辞。

## Expansion Sweep

### Future evolution

* JSON 使用显式 schema version 与稳定 case key，后续 P1 子任务可把 FIR/convolver/listening benches 接入同一格式。
* Baseline 输入保持普通文件接口，未来可由 CI base-branch artifact 或受控 runner 提供，无需改 benchmark CLI。

### Related scenarios

* 本 MVP 先覆盖与前四个子任务直接相关的 quality、callback 与 streaming-resampler；其他性能 bench 在对应 P1 子任务修改算法时迁移。
* Quality 与 performance 共享环境 metadata，但保留各自的 metric/case schema，避免制造没有意义的统一“音质分数”。

### Failure and edge cases

* Git/rustc/CPU 探测失败、dirty worktree、未知 feature、不可写输出路径、损坏 JSON、重复/missing case、NaN/Infinity timing、baseline 条件不兼容均需明确行为。
* Shared CI runner timing 只作为 artifact/report，10% 门禁仅在调用者提供兼容同机 baseline 时启用。

## Feasible Approaches

### Approach A: 先标准化 P0 相关三入口（Chosen）

* Quality 增补环境与 output timing/tail；callback/resampler 增补统计、JSON、baseline compare 与 CI quick artifacts。
* 保持任务边界可审核，并为后三个 P1 子任务提供可复用 support module。

### Approach B: 本任务一次迁移全部六个 benches

* 一次获得完全统一的报告格式。
* 改动和验证矩阵明显扩大，还会提前触碰 FIR/convolver 的后续算法任务，回归归因较差。

### Approach C: 只补文本统计，不接 CI/baseline artifacts

* 实现最快。
* 无法满足可追溯、持续门禁与 10% 同机回退策略，仍依赖人工复制输出。

## Decision (ADR-lite)

**Context**: 三个后续 P1 子任务将分别修改 FIR/AutoMix、listening/nonlinear 和 convolver 路径。若本任务预先迁移全部六个 benches，会扩大当前验证矩阵并让后续算法修复与报告重构互相干扰；若只补文本统计，又无法形成可追溯的持续门禁。

**Decision**: 采用 Approach A。本任务标准化 `audio_quality_measurements`、`audio_callback_chain_perf` 与 `audio_resampler_streaming_perf` 三个直接覆盖 streaming/P0 的入口，建立共享 bench support、JSON schema、trial distribution、兼容 baseline 比较和 CI artifact。其余 benches 在对应 P1 子任务中复用该基础逐步迁移。

**Consequences**: 当前改动保持在可审核范围内，并立即解锁三个依赖任务；短期内六个 bench 的输出格式仍非完全一致，但不会为追求表面统一而提前耦合尚未修复的算法。性能 hard gate 只在提供兼容 baseline 时启用，shared CI runner 继续把 absolute timing 作为 report。

## Technical Approach

* 在 `benches/support/` 放置只供 custom benches 使用的 serde metadata/statistics/report helpers；用集成测试直接 include 该模块，避免把 benchmark plumbing 暴露为 crate 公共 API。
* Trial 统计使用排序后的 nearest-rank p95，并保留原始 samples；模式通过降低单 trial iterations、增加 trials，在不显著扩大总工作量的前提下替代 best-of-N。
* Performance JSON 顶层包含 schema/probe/environment/mode/conditions/cases；baseline compare 先验证兼容条件和 case 集合，再逐 case 计算 `(candidate_median / baseline_median - 1) * 100`。
* Quality full-output point 直接传播 `RenderedOutput` metadata，不通过样本长度反推 latency/tail。
* CI 只强制报告完整性和确定性 quality gates；机器相关回退比较由显式 baseline artifact 激活。

## Out of Scope

* 引入 Criterion、Divan、数据库或网络 benchmark service。
* 在 GitHub shared runners 上设置跨机器 absolute ns/sample gate。
* 本任务原生迁移 convolver、FIR EQ、lock-free 或未来 P1 listening benches 的完整报告格式。
* 下载或重新分发 EBU Tech 3341/3342 corpus；本轮保留显式 skipped 和受控环境要求。
* 建立一个混合不同算法的“音质总分”或宣称全局最佳音质/性能。

## Implementation Plan

* PR1: shared environment/statistics/report support + deterministic unit tests.
* PR2: callback/resampler distribution、JSON、baseline compare、work validation。
* PR3: quality metadata/full-output fields、CI quick job、docs 与完整验证。

## Research References

* [`research/current-gate-gap-audit.md`](research/current-gate-gap-audit.md) — 当前 bench/CI 缺口、P0 回归覆盖矩阵与推荐 MVP。
* [`../07-16-audio-core-quality-correctness/research/audio-quality-gates.md`](../07-16-audio-core-quality-correctness/research/audio-quality-gates.md) — 分层音质证据与 oracle/阈值原则。
* [`../07-16-audio-core-quality-correctness/research/realtime-performance-gates.md`](../07-16-audio-core-quality-correctness/research/realtime-performance-gates.md) — median/p95、同机比较、deadline utilization 与零分配策略。

## Dependencies

* 前四个 P0/streaming 子任务（已完成）。
