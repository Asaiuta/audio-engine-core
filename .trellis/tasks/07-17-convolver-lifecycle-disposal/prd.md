# 修复 Convolver 生命周期与资源回收

## Goal

修复 canonical output-chain builder 隐藏 convolver disposal slot 导致的长期 kernel adoption 阻塞，建立显式、可观测且有界的控制线程发布/回收契约，同时验证统一 streaming 生命周期下卷积零延迟、有限 IR tail、finish/reset 与离线传播正确性。

## Parent / Dependencies

* Parent：`../07-16-audio-core-quality-correctness/prd.md`
* 已完成依赖：`07-17-variable-io-offline-finalize`、`07-17-audio-quality-performance-gates`、`07-17-fixed-dsp-streaming-migration`

## What I Already Know

* `ConvolverProcessor` 当前以 `owned + incoming + pending_retire + retired ArcSwap slot` 保持 audio-side 内存有界，并只在 kernel 唯一拥有时通过 `Arc::get_mut` 采纳，避免深拷贝。
* `OutputChainBuilder::build_callback_chain()` 只返回 `DspChain`，processor 内部创建的 `disposal_slot()` 句柄随类型擦除而不可达；canonical 调用方无法履行“控制线程 drain”契约。
* 未 drain 时 A/B/C 可依次形成 active/retired/pending，D 起 adoption 被延迟；后续 publisher 只会在 control-side swap slot 中 latest-wins，不会无限增长，但当前状态不可观测且可永久停滞。
* `FFTConvolver` 需要独占可变访问，控制端不能通过保留另一个强 `Arc` 简单避免析构；回收必须是真正的 ownership hand-off。
* 当前 convolver 报告零 algorithmic latency，finite tail 为 `IR frames - 1`；已有单声道 3-tap last-frame impulse 测试和 convolver→limiter→resampler 离线传播测试，但缺少多长度/多声道直接卷积 oracle 与长期 swap stress。
* Audio hot path 禁止分配、析构重资源、锁、日志、I/O、panic 与无界循环。

## Requirements (evolving)

### Control-plane lifecycle

* Canonical callback/offline/direct processor 入口使用同一个显式 convolver 控制面；不得再依赖类型擦除后不可达的内部 disposal handle。
* Kernel 构建、发布、被覆盖 kernel 的释放、retired kernel 回收都发生在 control/offline 线程；audio process 只做固定次数原子操作、ownership hand-off 与已有卷积计算。
* 发布策略在 audio 尚未 withdraw 时为 latest-wins；已 withdraw 的 incoming kernel 不得在 audio thread 丢弃或被深拷贝。
* 回收容量保持固定有界；耗尽时继续使用当前有效 kernel，将新 adoption 延迟，而不是在 callback 析构、分配或阻塞。
* Backpressure、等待回收、published/adopted/superseded/reclaimed 等关键状态通过 allocation-free atomic snapshot 可观测；状态语义必须能区分“尚未到下一个 block”和“回收通道持续未消费”。
* 控制 API 明确 one live audio consumer per control handle；多个 producer 如被允许，必须只在 control side 竞争且不能破坏唯一 kernel ownership。
* Cloned control handles may publish concurrently; control-side publication and reclamation are serialized for generation/install ordering, while audio never acquires that gate.

### Streaming timing / tail

* `ConvolverProcessor::latency()` 保持零；overlap-save/partitioned 实现的第一个非零输出与 direct convolution sample 0 对齐。
* Enabled 且已采纳 kernel 时，tail 精确为 `ir_length - 1` frames，并标记当前 sample-rate domain；disabled/no-kernel 为 `TailSpec::None`。
* `finish` 通过零输入精确产生剩余直接卷积样本，支持多次短 output block，最后一次 `Finished(n)` 后稳定 `Finished(0)`。
* `reset` 清除卷积 history、finish 进度和旧 stream 状态，但保留当前已采纳 kernel/control 配置；process-after-finish 仍遵守统一 trait 的 reset requirement。
* Offline finalize 继续把 convolution tail 完整传过 limiter/resampler，metadata 与 retained content 不受 block size 影响。

### Evidence / performance

* 长期 stress 覆盖快速 burst、control drain 比 audio block 更快/更慢、短暂 backpressure 后恢复、disable/reenable 与最终 latest kernel adoption；所有队列/slot/counter 有固定上界。
* 用独立直接卷积 reference 覆盖 mono/stereo、短/长 IR、普通 process + finish、irregular chunks、reset isolation 与 exact tail length。
* Audio-side publish adoption/retirement/backpressure 路径有析构探针与 `assert_no_alloc`，证明重 kernel destructor 只在 control thread 执行。
* Callback/convolver/FIR quick benchmarks 相对兼容同机 baseline 不得有超过 10% 的未解释 median 回退；无 baseline timing 只 report。

## Acceptance Criteria (evolving)

* [x] Canonical builder 构建后，调用方能保留并使用明确类型的 publish/reclaim/status handle；仓库内不再通过裸 `ArcSwapOption<FFTConvolver>` 拼接 canonical convolver 生命周期。
* [x] 至少 10,000 次不同更新节奏的 deterministic stress 后，最终发布 kernel 在有界 block/drain 次数内被采纳，status counters 与 superseded/backpressure 行为一致，无永久阻塞或未界定增长。
* [x] 回收通道满时 current kernel 连续处理；恢复 drain 后 adoption 自动继续；控制线程能明确观察 backpressure 与 pending reclamation。
* [x] Destructor-thread probe 和零分配测试证明 audio callback 不执行 kernel 重析构、不新增锁/分配/日志/I/O。
* [x] Direct convolution oracle 证明 process+finish 内容、`IR-1` tail、零 latency、irregular chunks 与 reset isolation；mono/stereo 和 overlap-save/partitioned 路由均覆盖。
* [x] Convolver→limiter→resampler 的 last-frame impulse、tail metadata 和不同 render block size 继续等价。
* [x] Callback、convolver、FIR quick gates 与完整 fmt/Clippy/all/no-default test matrices 通过；首次 FIR 运行的机器波动已复跑确认，未发现未经批准的显著性能回退。

## Research References

* [`research/reclamation-design.md`](research/reclamation-design.md) — 分析 unique ownership 限制、显式控制 handle、返回 reclaimer 与后台线程三种方案。
* [`research/timing-tail-evidence.md`](research/timing-tail-evidence.md) — 零 latency、`IR-1` finite tail、direct convolution oracle 与 finish/reset 测试矩阵。
* [`research/performance-baseline.md`](research/performance-baseline.md) — 当前 HEAD 的 callback、FIR 与 convolver quick 基线和复现命令。

## Feasible Approaches

### Approach A：显式 `ConvolverControl`（推荐）

* 封装 incoming publication、enabled、retired hand-off 与 atomic status；control-side `publish` 对 kernel 取得唯一 ownership，并 opportunistically reclaim/supersede。
* `ConvolverProcessor`、`OutputChainParams`、callback builder 与 offline chain 统一持有该 handle；调用方可显式 `reclaim_retired()` 和读取 snapshot。
* 优点：错误用法最少、三入口语义一致、无需新线程/依赖、backpressure 可测。
* 代价：公开构造/参数 API 有一次破坏性调整；若控制端永不 reclaim，仍按明确策略有界 backpressure。

### Approach B：builder 返回 `(DspChain, ConvolverReclaimer)`

* 保留裸 swap/enabled publisher，只让 builder 把当前 retired slot 一并返回。
* 优点：改动最小。
* 代价：发布和回收仍是两套松散句柄，调用方容易忘记 drain；direct/offline 入口继续不一致，状态观测弱。

### Approach C：库内后台 reclaimer thread

* Builder 隐式启动线程持续消费 retired slot，chain drop 时停止/join。
* 优点：host 几乎不会忘记回收，短期 adoption latency 有界。
* 代价：线程生命周期、关闭时序、平台调度和隐藏资源成本复杂；核心 DSP crate 当前没有隐式 worker，且不能替代满载时的显式 backpressure 契约。

## Expansion Sweep

* Future evolution：control snapshot 可为宿主 UI/诊断提供“published/adopted/backpressured”状态；未来如需 crossfade kernel transition，可复用 generation/status，而不把 crossfade 纳入本次。
* Related scenarios：callback、offline render 与 direct adapter 必须共享 ownership contract；FIR EQ 内部固定 kernel 不进入动态 publication 控制面。
* Failure/edges：producer burst、producer 暂留引用、disable 时仍有 incoming、retired slot 满、sample-rate/reset/finish 交错、chain/control drop 顺序均需定义。

## Decision (ADR-lite)

**Context**：隐藏的单槽 disposal 机制本身满足 audio-side 有界/无重析构，但 canonical builder 类型擦除后没有控制端能够消费；仅扩大队列只会推迟相同故障，后台线程则给核心库引入隐式调度与关闭生命周期。

**Decision**：采用 Approach A。新增显式、cloneable、single-audio-consumer 的 `ConvolverControl`，统一封装 kernel-by-value publish、enabled、retired reclaim 与 allocation-free status snapshot。`OutputChainParams`、direct adapter、callback/offline builder 统一使用该控制面；不保留 canonical 裸 swap/enabled 拼装路径。

**Consequences**：公开构造/参数结构发生一次直接切换；control caller 必须周期性调用 reclaim（publish 会 opportunistically 执行），否则系统保持有界并明确 backpressure、继续使用 current kernel。不会新增后台线程、锁、无界队列或卷积算法变化。

## Technical Approach

* 在 adapter 层定义 `ConvolverControl`、copyable `ConvolverStatus` 和必要的原子计数；publication 接收 `FFTConvolver` 所有权，先/后 opportunistic reclaim，并在 control thread latest-wins 替换未 withdraw kernel。
* `ConvolverProcessor` 只持有 control clone 与固定 ownership stages；每 block 至多执行固定次数 flush/withdraw/adopt，回收容量满时设置 backpressure 并保持 current kernel。
* `OutputChainParams` 用单一 control handle 替换公开裸 `convolver_swap`/`convolver_enabled`；builder 提供 control accessor，offline 调用点在非 RT 边界主动 reclaim。
* 独立 nested-loop direct convolution oracle 验证 process+finish、零 latency、`IR-1` tail、分块与 reset；长期 stress/status/destructor-thread tests 验证资源生命周期。
* 复用 callback/FIR versioned baseline gate，保留 `audio_convolver_perf --quick --enforce` 作为 inner algorithm guard；本任务不改 routing constants。

## Implementation Plan (small commits)

1. 新增 control/status contract，迁移 direct `ConvolverProcessor` 并加入 backpressure/reclamation stress 与 destructor-thread oracle。
2. 迁移 `OutputChainParams`、callback/offline builders、benches/tests/exports，消除 canonical hidden disposal path。
3. 扩充 direct convolution timing/tail/reset evidence，复跑 baseline gates并更新 spec/docs/PRD。

## Definition of Done

* PRD 选择已确认并记录 ADR-lite。
* 缺陷机制先有旧实现失败的 regression/stress，再修复根因。
* Tests、strict Clippy、rustfmt、feature matrices 与相关 quick gates 全绿。
* 新 lifecycle/status API、失败/降级语义、直接卷积证据和性能数据写入 docs/spec/research。
* Trellis check、spec update、工作提交、归档和 journal 完成。

## Out of Scope

* Kernel-to-kernel audio crossfade、IR morphing、异步 IR 文件加载或卷积算法/partition threshold 重设计。
* 为所有 DSP processor 引入通用后台垃圾回收器或全局 executor。
* 无测量依据地改变 overlap-save/partitioned routing、FIR EQ kernel ownership 或声学响应。
* 保证控制线程永不调用 reclaim 时仍无限接受所有 kernel；MVP 必须有界并显式报告 backpressure。

## Technical Notes

* 主要文件：`src/processor/adapters.rs`、`output_chain.rs`、`mod.rs`、`lib.rs`、相关 tests/benches 与 streaming/realtime specs。
* 当前 `DspChain` 使用 `Box<dyn StreamingProcessor>`，因此 processor-specific handle 必须在类型擦除前由 builder/control plane 保留。
* `ArcSwapOption::store` 会释放被替换值；任何可能替换重 kernel 的 store/swap 只能在 control/offline 线程执行，audio 线程只可向确认为空的 retirement slot hand off。
* 任务复杂度：Moderate/architectural；不需要拆新 child task，但需要先确认 control ownership 方案。
