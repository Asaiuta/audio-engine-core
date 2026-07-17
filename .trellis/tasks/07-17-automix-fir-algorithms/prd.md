# 修复 AutoMix 与 FIR EQ 算法

## Goal

修复 AutoMix 时间基准与 FIR EQ 退化配置、统一增益和 minimum-phase window 的 P1 算法错误；对当前没有实现与真实语料证据的 key 能力给出机器可读、版本化的 unsupported 契约，避免以恒为 `None` 的字段暗示功能可用。

## Parent

* `../07-16-audio-core-quality-correctness/prd.md`

## Requirements

* BPM 换算使用实际 observation cadence：spectral flux 为 `sample_rate / 512`，50 Hz 只用于 envelope fallback；lag 搜索边界由保留的约 55–200 BPM 支持范围和实际 cadence 推导，不再把 15..55 固定 lag 套到所有采样率。
* 用 50 Hz、44.1 kHz/512-hop 与 48 kHz/512-hop 的已知脉冲 fixtures 验证 tempo；无效 rate、平坦/过短输入继续返回结构化低置信结果。
* AutoMix DTO schema 版本递增，并新增公开、可序列化的 key-analysis 状态；本任务返回 `unsupported`，现有 key result 字段必须为 `None`。未来只有在独立标注语料验证后才能新增 detected/insufficient-evidence 状态。
* FIR 1-tap 明确定义为 1 kHz 参考处的纯标量：0 dB 是单位 impulse，统一 `g` dB 是 `10^(g/20)`；linear/minimum 两种 phase mode 都必须 finite。
* FIR 频率响应保留绝对增益；全频统一正/负增益不得被 1 kHz 反向归一化回 unity。
* 修正 minimum-phase tail raised-cosine window 的方向，使其从中点附近的 1 单调衰减到末端 0，并以独立频响计算与能量质心 oracle 验证。
* `audio_fir_eq_perf` 复用现有 bench support，输出 versioned JSON、环境/条件、raw trials、median/p95/max，并支持同环境 baseline 下默认 10% median 回退门禁；shared runner 无 baseline 时绝对 timing 保持 report-only。
* 不改变 `FFTConvolver` 路由阈值、partition size 或 callback 热路径；FIR 设计仍是 control/offline 路径。

## Acceptance Criteria

* [x] 60/120/180 BPM 的确定性 fixtures 在 50 Hz、44.1 kHz/512-hop、48 kHz/512-hop cadence 下落入最大 2% 相对误差，且回归测试能捕获旧 50 Hz 错用。
* [x] 序列化结果包含递增 schema version 与 `key_status = "unsupported"`；四个 key result 字段均为空，API 文档说明 root/mode 字段仅为未来保留。
* [x] 1-tap、统一 ±6 dB、minimum/linear phase 全部 finite；0 dB one-tap 等于 `[1]`，±6 dB one-tap 幅度误差不超过 `1e-12`。
* [x] 代表性曲线在 ISO band probes 的独立 DFT 频响误差命中实施期校准并记录的保守阈值；uniform ±6 dB 响应误差不超过 `1e-9 dB`。
* [x] Minimum-phase IR 的能量质心显著早于对应 linear-phase IR；tail taper 从 1 单调降到 0，旧反向公式会使测试失败。
* [x] FIR 性能报告通过 schema/work-integrity 检查；兼容 baseline 下各 case median 回退不超过 10%，或取得明确批准。
* [x] 双 feature 测试/Clippy、rustfmt、rustdoc 与 package 验证通过。

## Technical Approach

* 将 spectral-flux hop 定义为单一常量，由 accumulator 位移与 tempo cadence 共同使用；`finalize_analysis` 根据实际选择的序列传入匹配 rate。
* `detect_bpm` 按 BPM 上下限计算合法 lag，并保持现有无足够证据时的 `Option` 契约；不在本任务引入第三方 tempo/key 依赖。
* 新增可扩展的 `AutomixKeyStatus` 公共 enum，版本 2 当前只承诺 `Unsupported`；保留四个 nullable key 字段以减少下游迁移并为未来 corpus-backed 结果预留载荷。
* FIR generation 在一 tap 时走显式 scalar fast path；一般路径移除反向 1 kHz normalization，并把 minimum-phase taper提取为可直接验证的纯函数。
* 单元测试使用独立公式计算 tempo fixture、FIR DFT 响应与能量质心；性能 bench 只复用 bench-local support，不把报告模型暴露为 crate API。

## Decision (ADR-lite)

**Context**: 当前 key 字段恒为 `None`，但仓库没有 key estimator、独立标注语料或准确率门禁。快速加入 FFT chroma/profile correlation 只能证明合成和弦，不能支撑真实歌曲 key 准确率；另一方面直接删除字段会造成更大的公共 DTO 迁移。

**Decision**: 本任务新增版本化 `AutomixKeyStatus::Unsupported`，继续保留 nullable payload 字段但明确禁止把它们解释为已运行却低置信。Tempo 与 FIR 按解析契约完整修复；真实 key detection 另立 corpus-backed 任务。

**Consequences**: AutoMix 输出不再误导调用方，且未来可区分 unsupported、insufficient evidence 与 detected；本轮不会虚构“最佳 key 算法”或新增未经验证的计算开销，代价是仍不提供实际调性结果。

## Out of Scope

* 实现或宣称真实歌曲 key detection、下载/分发标注音乐 corpus、做 MIREX/GiantSteps 级准确率比较。
* 重写 tempo detector 为 beat-tracking/动态规划系统，改变现有约 55–200 BPM 产品范围，或解决所有 half/double-tempo 歧义。
* 改变 10-band 插值模型、tap 路由阈值、convolver 实现或 FIR apply 热路径。
* 把所有剩余 legacy benches 一次性标准化；本任务只迁移直接受影响的 `audio_fir_eq_perf`。

## Research References

* [`research/algorithm-contracts.md`](research/algorithm-contracts.md) — 实际 tempo cadence、key 证据边界、FIR 解析语义与测试 oracle。

## Implementation Plan

* PR1: AutoMix cadence/lag 修复、key unsupported schema 与确定性测试。
* PR2: FIR one-tap/absolute gain/tail window 修复及频响/能量 oracle。
* PR3: FIR performance report/baseline 门禁、验证证据与规范更新。

## Dependencies

* `07-17-audio-quality-performance-gates`
