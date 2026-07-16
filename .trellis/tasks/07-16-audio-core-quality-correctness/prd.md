# 修复音频核心正确性、音质与实时性能问题

## Goal

基于本轮代码审查与实测探针，修复会造成音频丢失、状态串音、瞬态不连续、离线输出截尾或配置失真的核心问题，并为音质算法与实时性能建立可执行的回归标准。目标不是宣称抽象意义上的“全球最高性能/最佳音质”，而是让每项结论都有正确性测试、客观音频指标和可复现基准支撑。

## What I already know

### Baseline verification

* `cargo fmt --all -- --check` 已通过。
* Clippy（`--all-features` 与 `--no-default-features`）已通过。
* 测试基线：all-features 229 项通过；no-default-features 221 项通过。
* 当前 512-frame 回调基准：启用 DSP、无卷积约 `143.4 µs`；加入 256-tap 卷积约 `154.4 µs`；lock-free snapshot 约 `7.54 ns/read`。
* 现有 quality `--enforce` 能通过，但 EBU 测试语料缺失，完整输出 true-peak 仍仅报告（本轮结果约 `-0.610 dBTP`），因此“通过”不足以证明所有音质路径正确。

### P0 correctness / data-integrity findings

* Resampler 没有消费 SoXR 返回的 `Processed.input_frames`，flush 使用 `process(&[])` 而不是原生 `drain()`，reset 未调用原生 `clear()`。探针中 48→96 kHz：512 输入帧 flush 后输出 0；100,000 输入帧只输出 31,540；reset 后仍泄漏上一段音频，峰值约 `0.80086`。
* EQ 参数切换的 crossfade 完成后只复制目标系数、丢弃目标滤波器的内部状态；探针测得切换边界线性误差约 `1.4425`。
* 离线渲染未排空 limiter 的 look-ahead 延迟/尾部；位于最后一帧的 impulse 会从最终输出中完全消失。
* `LoudnessConfig.enabled` 与 `mode` 未传播到 atomic state；探针中 `enabled=false, Album` 被还原为 `enabled=true, Track`。
* Dynamic loudness 的 RBJ shelf 公式对 `beta` 额外乘了一次 `sin(w0)`；builder 设置 sample rate 时重建处理器并丢失 volume/strength。

### P1 algorithm / quality findings

* AutoMix spectral-flux 实际更新率约 86–94 Hz，但 BPM 换算固定假设 50 Hz；key 结果字段始终为 `None`。
* FIR EQ：1-tap 配置产生 NaN；全频段 +6 dB 被归一化回 unity；minimum-phase tail window 方向反转。
* Saturation 在 threshold 处传递函数不连续，可能制造额外宽带瞬态。
* Noise shaper 对低于 -120 dBFS 的输入硬门控，且可能输出超出 `[-1, 1]`。
* 标记为 Bauer-style 的 crossfeed 使用高通交叉馈送，和该模型通常预期的低频交叉馈送方向相反。
* Canonical output-chain builder 隐藏 convolver disposal slot，长期运行后可能阻塞新卷积核的接纳。

## Assumptions

* 这是复杂修复任务，应先按风险分层并用小批次变更完成，而不是一次性重写 DSP 架构。
* 位精确兼容不是默认目标；在修复数学错误或状态错误时允许输出发生可解释、可测量的变化。
* 用户已选择统一重构 streaming trait 并直接切换；允许为获得一致生命周期语义而调整现有公共处理器 API，不保留兼容窗口。
* 实时线程继续遵守无锁、无阻塞、无非预分配堆分配的约束。

## Requirements

* 当前任务作为总父任务保留全部 P0/P1 审查发现；实现工作拆为分层子任务，避免一次性大改。
* 第一实施批次只处理 P0 正确性与数据完整性阻断项；P1 音质、算法及长期运行问题进入后续独立子任务。
* 子任务按依赖关系执行：先稳定流式状态、drain/reset 与配置传播契约，再处理依赖这些契约的算法和质量优化。
* 建立统一的流式 DSP 生命周期契约，至少覆盖输入消费量、输出生产量、算法延迟、尾部长度、drain/finalize、reset 及重复调用行为。
* 所有音频处理器迁移到统一 streaming trait；固定帧数原地处理器也通过同一契约表达，并可保留无额外拷贝的优化路径。
* DSP 内核 buffer 固定为 interleaved `f64`；以零拷贝 `AudioBlockRef` / `AudioBlockMut` view 封装 samples、channels 与 frames，并在边界验证完整帧。
* 文件、decoder 或设备的 `f32`/整数格式只在进入和离开 DSP graph 时转换一次；本次不把每个 processor 泛化为多 sample type 或 planar layout。
* 统一 trait 以安全 buffer enum 同时表达 in-place 与 out-of-place 调用：固定 1:1 stage 使用原地快速路径，可变 I/O stage 使用独立 caller-owned output，分派不得进入逐样本循环。
* 统一 trait 使用 caller-owned input/output storage 并返回明确的 consumed/produced 进度；不得依赖动态增长输出缓冲来表达尾部或采样率变化。
* Chain、adapters、离线 renderer、直接 processor 调用、tests、examples 和 benches 必须迁移到同一生命周期模型，不再由 resampler/limiter 私有约定补洞。
* 本次直接移除公开的旧 `AudioProcessor` / `ProcessResult` 契约，不提供 deprecated wrapper 或 feature-gated 兼容实现。
* README/API docs/CHANGELOG 必须提供旧 `process(&mut [f64], channels)` 到新 input/output/progress/lifecycle API 的迁移示例，并明确这是 breaking change。
* 离线 renderer 默认补偿整条链的算法延迟，使源事件在目标时间轴上保持对齐，同时保留卷积等效果产生的可听尾部。
* Streaming API 另提供显式 raw causal render 模式，输出未裁剪的前置延迟与完整 finalize 数据，供需要自行补偿的调用方使用。
* 生命周期模型必须区分 algorithmic latency 与 semantic effect tail；不得把 limiter/resampler 的延迟排空误算为需要延长成片时长的效果尾音。
* 跨采样率链路的延迟补偿统一换算到最终输出帧时间轴，并记录舍入规则，避免逐阶段取整造成累计偏移。
* 对明确有限的 tail 使用处理器声明的精确帧数；对无限或无法精确预报的 tail，使用 dither/quantization 前的能量阈值、连续静音保持时长与最大尾长共同终止。
* 未知 tail 的静音阈值、保持时长和安全上限均可由 render policy 配置，并提供保守默认值；达到上限时返回 `tail_truncated=true` 及实际渲染帧数，不得伪装成完整成功。
* Tail detector 的停止条件不依赖 block size；改变离线分块大小不得显著改变终止时间或保留内容。
* 建立可持续质量门禁：缺陷机制回归、解析或独立参考 oracle、完整输出 true-peak/响度/频响/连续性指标，以及缺失外部语料时的显式降级状态。
* 建立可持续实时性能门禁：固定测试场景、预热与多次采样、修复前后对比、允许波动范围，以及实时线程无锁/无阻塞/无热路径分配检查。
* 每个纳入范围的缺陷先添加能在旧实现上失败的最小回归测试，再修复根因。
* 流式与一次性处理在相同输入、分块方式不同时必须保持定义明确且可验证的等价性。
* 所有有内部延迟或尾部的处理器必须提供并正确使用 drain/finalize 语义；reset 后不得泄漏上一流的状态。
* 系数切换必须同时维护正确的滤波器状态，不得在切换边界引入非预期脉冲或幅度跃变。
* 配置对象、atomic snapshot 与实际处理器状态必须完整往返，不能静默回退到默认值。
* 算法修复需要用解析参考、独立参考实现或客观频响/连续性指标验证，而不只验证“输出有限值”。
* 性能修复与正确性修复均不得破坏实时线程安全约束；关键回调基准需与当前基线对比。

## Acceptance Criteria

* [ ] Resampler 对不同输入长度、上下采样比例与随机分块均不丢帧；drain 后输出长度符合延迟补偿后的明确契约；reset 隔离前后音频流。
* [ ] EQ 参数切换在连续信号、impulse 与分块输入下无状态丢失；相对“目标滤波器持续积累状态”的 `f64` reference，切换边界最大线性误差不超过 `1e-9`。
* [ ] 离线渲染能保留末尾 impulse，并明确处理 limiter/resampler/convolver 等所有有状态阶段的尾部。
* [ ] Loudness 配置的 `enabled`、`mode` 及相关字段可完整传播和往返。
* [ ] 纳入范围的数学/音质算法均有针对其缺陷机制的回归测试与客观指标。
* [ ] `cargo fmt --all -- --check`、Clippy、all-features/no-default-features 测试矩阵全部通过。
* [ ] 512-frame 关键实时基准无未经批准的显著回退，并记录修复前后数据及测试环境。
* [ ] quality gate 补足本次缺陷覆盖；若外部 EBU 语料仍不可用，明确记录未验证项而不宣称“最佳音质”。
* [ ] 生命周期契约包含 consumed/produced、latency/tail、幂等 drain/finalize 与彻底 reset，并有一次性/随机分块/末帧 impulse 性质测试。
* [ ] 预热后的实时 process、snapshot 及 callback-facing 结束路径通过零分配检查；不存在新增 lock、I/O、logging、panic 或无界循环。
* [ ] 性能报告至少包含 median、抖动统计、deadline utilization 与环境元数据；相同条件下 median 回退超过 10% 必须修复或取得明确批准。
* [ ] 所有仓库内实现、chain、examples、benches 和文档迁移到新 trait；除迁移说明外不再引用旧 `AudioProcessor` / `ProcessResult`。
* [ ] 默认离线渲染中的首个 impulse 与目标时间轴对齐；末帧 impulse 不丢失；有限卷积尾部完整保留。
* [ ] Raw causal 模式保留已声明的前置延迟和 finalize 输出；默认补偿模式与 raw 模式在扣除 latency 后信号内容一致。
* [ ] 未知 IIR tail 在低于阈值并满足保持时长后稳定终止；持续非静音尾部达到上限时标记 `tail_truncated`，且不同 block size 得到等价结果。
* [ ] `AudioBlockRef/Mut` 为零拷贝 view；不完整 interleaved frame、零 channels 或容量不足均得到明确结果，不再静默截断输入。
* [ ] 固定 1:1 callback chain 继续走 in-place 路径；可变 I/O 使用预分配 scratch，二者预热后均无分配。

## Definition of Done (team quality bar)

* Tests added/updated (unit/integration where appropriate)
* Lint / typecheck / CI green
* Docs/notes updated if behavior changes
* Rollout/rollback considered if risky
* 正确性、音质和实时性能结论均附可复现命令与测量结果

## Out of Scope

* 未经测量依据的全量 DSP 重写或依赖替换。
* 仅凭主观听感宣称“最高性能”“最佳音质”或“最佳算法”。
* 与上述发现无关的 UI、播放列表、网络或产品功能改动。
* P1 项不进入第一批 P0 实施子任务，但继续由本父任务跟踪并拆入后续子任务。
* 首批 P0 子任务不顺带重写无关 DSP；通用生命周期和质量门禁作为独立支撑子任务，以明确依赖接入各修复批次。
* 不提供旧 trait 的一个版本弃用期，也不增加 legacy compatibility feature。
* 本次不原生泛化 `f32`、整数 sample type、planar layout、GPU buffer 或设备专用 buffer；如未来需要，另建带独立基准和音质验证的任务。

## Technical Notes

* 主要涉及：`src/processor/resampler.rs`、`eq.rs`、`output_chain.rs`、`loudness/*`、`dynamic_loudness.rs`、`automix_analysis.rs`、`fir_eq.rs`、`saturation.rs`、`crossfeed.rs`、`convolver.rs`、`adapters.rs`、`lockfree_params.rs`。
* 性能基准入口包括 `benches/audio_callback_chain_perf.rs`、`benches/audio_convolver_perf.rs`、`benches/audio_fir_eq_perf.rs`、`benches/audio_lockfree_params_perf.rs`。
* 影响面检查：`AudioProcessor`/`ProcessResult` 是公开 re-export；现有 8 个 adapter 实现该 trait，`DspChain` 存储 `Vec<Box<dyn AudioProcessor>>`，offline output chain、quality/callback benches、模块文档与大量 adapter tests 直接依赖旧签名。
* 仓库是单 crate `audio-engine-core`，当前版本 `0.1.0`，未声明 Cargo workspace members；仓库内没有需要跨 package 协调的消费者，但仓库外消费者是否存在无法从本地得知。
* 任务复杂度：Complex。需要先收敛 MVP 范围，再研究各算法的参考契约、阈值与测试 oracle。
* 当前任务保持 `planning` 状态；PRD 未确认前不运行 `task.py start`。

## Research References

* [`research/stream-lifecycle-contract.md`](research/stream-lifecycle-contract.md) — SoXR、Rubato 与 VST3 均把消费/生产进度、延迟、尾部和 reset 作为显式契约；文末按用户选择补充了统一 trait 下的安全 in-place/out-of-place buffer 设计。
* [`research/audio-quality-gates.md`](research/audio-quality-gates.md) — 采用数据完整性、数学 oracle、客观指标、外部 corpus 和主观试听五层证据；不建立笼统“音质总分”。
* [`research/realtime-performance-gates.md`](research/realtime-performance-gates.md) — 建议演进现有 custom harness，以零分配硬门禁、同机相对比较、median/p95 与 callback deadline utilization 取代跨机器绝对 ns 断言。

## Technical Approach

### Unified streaming core

* 引入零拷贝 interleaved `f64` block views、对象安全的统一 streaming processor trait、`ProcessProgress` 与明确的 backpressure/finished 状态。
* 安全 buffer enum 提供 in-place 与 out-of-place 两个调用形态，但共享完全相同的 progress、finish、latency/tail 与 reset 生命周期；不使用 unsafe alias 制造“伪原地”接口。
* 输出容量耗尽属于正常 `NeedOutput`；在具备输入和输出容量时连续零进度属于契约错误，chain 必须返回错误而不是无限循环。
* `finish` 可重复调用且最终稳定返回 finished/0；`reset` 清除 processor、adapter、chain 及 SoXR native state。

### Chain and render composition

* 固定 1:1 callback stage 保持 in-place；resampler 等可变 I/O stage 使用预分配 scratch/ping-pong buffer，避免统一抽象造成整条实时链额外复制。
* Finalize 按拓扑顺序传播：上游产生的尾部继续通过所有下游 stage，然后逐级完成下游自身尾部。
* Trait 分别报告 algorithmic latency 与 semantic tail。默认 offline policy 在最终输出时间轴一次性补偿累计 latency，并保留效果 tail；raw policy 不裁剪。
* 有限 tail 精确排空；未知 tail 在 dither 前以可配置能量阈值/静音保持时长/最大尾长终止。初始默认建议为 `-120 dBFS` peak、`250 ms` hold、`30 s` maximum，实施时用确定性 fixtures 校准并记录依据。

### Correctness, quality, and performance evidence

* 每个旧探针转为“旧实现必失败”的回归测试；随机 chunk/property tests 覆盖 consumed/produced、finish、reset 和 block-size invariance。
* 数学算法使用 RBJ 解析频响、直接卷积、已知 BPM fixtures 或持续状态 reference，而不是只断言 finite。
* Quality bench 保持 gate/report/skipped 分类；完整输出 tail/length/true-peak 增加可追踪指标，缺失 EBU corpus 继续显式 skipped。
* Performance bench 记录环境、median/p95 和 deadline utilization；零稳态分配为硬门禁，相同环境 median 回退超过 10% 必须修复或明确批准。

## Planned Child Tasks / Small PRs

1. **统一 streaming contract**：block views、buffer enum、progress/error、latency/tail、finish/reset、旧 trait 直接移除及迁移文档骨架。
2. **固定 DSP 与 callback chain 迁移**：8 个 adapters、`DspChain`、原地快速路径、零分配与 callback 基准。
3. **可变 I/O 与离线 finalize**：SoXR consumed/produced、native drain/clear、chain tail propagation、latency compensation、raw/default render policy。
4. **P0 状态与数学正确性**：EQ target state、loudness config、dynamic-loudness RBJ 公式及 builder 状态保留。
5. **质量与性能门禁**：生命周期性质测试、完整输出指标、median/p95/deadline 报告与基准证据。
6. **P1 分析/FIR 算法**：AutoMix rate/BPM/key 与 FIR 1-tap、统一增益、minimum-phase window。
7. **P1 listening/nonlinear DSP**：saturation 连续性、noise-shaper 低电平/边界、Bauer crossfeed 模型与对应 oracle。
8. **P1 convolver 生命周期**：disposal slot 暴露、长期 kernel adoption/backpressure stress 与资源回收验证。

## Feasible Lifecycle Approaches

### Approach A：双层兼容契约（研究阶段推荐，未选择）

保留 `AudioProcessor` 作为固定帧数、原地实时接口；新增 `ProcessProgress`、latency/tail 查询和 caller-owned `drain_into`/`finish_into` 能力，由离线 render chain 按阶段传播尾部。旧便捷 API 保留并委托新实现。

### Approach B：扩展现有 trait 的默认方法

给 `AudioProcessor` 增加零延迟/零尾部/空 drain 默认方法。迁移较少，但原地 `process` 仍不能表达 resampler 的部分消费与输出扩张，最终仍需第二套特殊接口。

### Approach C：破坏性统一 streaming trait

全部 processor 改为 input/output + consumed/produced + finish/reset。抽象最统一，但会大范围改变公共 API、adapters、bench 和 callback 路径，超出修复 P0 所需风险。

**Chosen**：用户选择本方案，接受扩大迁移范围以换取统一、可组合的生命周期语义。

## Decision (ADR-lite)

**Context**: 审查发现横跨流式生命周期、实时状态、离线渲染及多个独立 DSP 算法。若全部塞入单个实现批次，回归归因、基准对比和审核都会变得困难；若只记录 P0，则 P1 发现容易丢失。

**Decision**: 使用当前任务作为总父任务，完整保留 P0/P1 清单；优先建立 P0 正确性子任务，P1 按算法/质量主题拆为后续子任务；另设支撑子任务统一 DSP 生命周期契约，并建立客观音质与实时性能门禁。

**Consequences**: 可以先消除数据丢失与状态错误，让同类状态/尾部缺陷有统一预防机制，并使“音质/性能”结论可持续复测；代价是需要维护子任务依赖、稳定测试环境和多轮质量验证。

### Lifecycle API Decision

**Context**: 双层兼容方案风险更低，但会长期保留固定原地与可变 I/O 两种抽象；用户更重视统一模型与长期算法正确性。

**Decision**: 将所有 processor、chain 与 render path 统一迁移到 streaming trait，以 caller-owned buffers、consumed/produced、latency/tail、finish/drain 和 reset 表达完整生命周期。

**Consequences**: 能从类型与编排层阻止静默丢输入、漏尾部和 reset 不彻底；同时属于公共 API 级重构，必须增加编译期迁移覆盖、实时零分配测试、等价性基线和清晰迁移文档。

### Legacy API Migration Decision

**Context**: crate 当前为 `0.1.0`，仓库内消费者可在同一变更中迁移；保留兼容层会让新旧生命周期语义并存，并削弱统一 trait 的约束力。

**Decision**: 直接切换到新 streaming trait，移除旧 `AudioProcessor` / `ProcessResult`，不提供 deprecated 或 feature-gated 兼容层。

**Consequences**: 实现和内部调用路径更干净，避免长期维护双模型；仓库外消费者必须一次性迁移，因此发布说明、API 示例和精确的 breaking-change 清单属于验收要求。

### Offline Render Timing Decision

**Context**: 统一 finalize 会同时释放 look-ahead/resampler 等算法延迟，以及 convolution 等具有语义的效果尾部。若一律保留会在文件开头引入延迟；若一律裁剪又会丢失尾音。

**Decision**: 默认离线模式补偿整条链的 algorithmic latency，并保留 semantic effect tail；同时公开 raw causal 模式供调用方自行处理时间轴。

**Consequences**: 默认结果与源时间轴对齐且不截断可听尾部；trait、chain composition 和测试必须分别追踪 latency/tail，并正确处理跨采样率帧单位与舍入。

### Unknown Tail Termination Decision

**Context**: 有限 convolution/limiter/resampler tail 可以精确声明，但 IIR 或未来效果可能只有渐近衰减或无法预报终点；无限 drain 会挂死离线渲染，固定时长又可能静音过多或截断。

**Decision**: 对未知/无限 tail 采用能量阈值 + 连续静音保持时长 + 可配置安全上限；检测发生在 dither/quantization 之前，触及上限时显式返回截断元数据。

**Consequences**: 离线渲染保证终止并能保留可听衰减；需要 block-size-independent detector、默认参数依据、截断状态传播及相应边界测试。

### Buffer Model Decision

**Context**: 同时泛化 sample type 与 planar/interleaved layout 会把生命周期修复扩大为全 DSP 格式框架重写。当前公开处理链和算法以 interleaved `f64` 为主，边界转换已经能覆盖文件与设备格式。

**Decision**: Streaming core 使用零拷贝、类型化的 interleaved `f64` `AudioBlockRef/Mut`；多种 sample format 只在 DSP graph 边界转换。统一 trait 通过安全枚举支持固定 stage 的 in-place 快速路径和可变 stage 的 out-of-place 路径。

**Consequences**: 保持当前数值精度、对象安全与实时性能，显著缩小迁移矩阵；本轮不获得原生 `f32`/planar DSP，但未来可在独立、基准驱动的任务中扩展边界或增加专用 graph。
