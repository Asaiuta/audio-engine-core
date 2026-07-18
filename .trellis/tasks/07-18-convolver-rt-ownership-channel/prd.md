# 修复 Convolver 实时安全、生命周期与输出链一致性

## Goal

修复审查确认的五项问题：首先消除音频线程上的 ArcSwap 分配、无界 writer 扫描和
重 kernel 析构风险，再修正 finish/disable、单消费者、状态一致性与输出链结构漂移，
使实现真正满足仓库声明的硬实时与 streaming 生命周期契约。

## What I Already Know

* P0 缺陷来自 `retired.load/store` 与 `published.swap` 在音频线程使用 ArcSwap
  DefaultStrategy；它不是单纯“预热即可解决”的问题，writer debt traversal 仍不
  具固定上界。
* partial finish 后禁用会移走 active kernel，但保留非零 remaining，确定性返回
  `active kernel disappeared during finish`。
* single audio consumer 仅写在文档中；cloneable builder/control 可安全编译出多个
  消费者，并破坏 publication/retirement 的单写者假设。
* `audio_idle` 存在并发 lost update，但完整 quiescence 还有其他保护条件；它应作为
  状态一致性修复，而不是与 P0 混为同一故障。
* adapters 模块过大且 output-chain stage 顺序多处手写；Meter 是 metadata/reporting
  语义不一致，不是缺少音频信号变换。
* 现有 17 项 Convolver 测试全部通过，但缺少能捕获上述机制的对抗性用例。
* 用户已确认 P0 hand-off 采用双向 `AtomicPtr` ownership slots；SPSC ring 与 index
  pool 不进入本任务实现。
* 用户已确认 output-chain 采用私有 declarative stage manifest；offline 不改为统一
  trait-object 容器。
* 用户已确认 quiescence 采用 publication generation + audio drained-generation
  acknowledgement；packed state word 与无版本 audio enum 不进入本任务实现。
* 用户已确认采用 targeted Convolver vertical module；其余七个 adapter 实现不重写，
  仅将现有非 Convolver tests 机械外移。

## Requirements

### P0 - hard-RT ownership channel

* 音频线程不得调用 ArcSwap load/store/swap，不得产生首次 TLS/Box 分配、引用计数
  最后释放或随全局线程数增长的 writer 遍历。
* publication/reclamation 使用固定容量、严格 O(1) 的唯一 ownership hand-off；
  control 多发布者仍只在 control-only gate 上串行化。
* 保留 latest-wins、当前 kernel 连续处理、有界 backpressure、generation/status 与
  control-thread destruction 语义。
* 新音频 OS 线程第一次 process/finish 即必须通过 no-allocation/no-deallocation
  断言，不允许靠同线程 control-side 预热。

### P1 - lifecycle and consumer ownership

* finish 一旦开始，锁定当前 kernel 直至已声明的 `IR length - 1` tail 完整排空；
  中途 disable 不得返回 Backend error 或截断已承诺 tail。
* control publisher 可克隆；audio consumer lease 不可克隆。第二个 live consumer
  必须在构建/构造时以明确错误拒绝。
* callback、offline render 与 direct processor 使用同一 lease 规则；需要并存时必须
  使用不同 control。
* lease 为内部实现细节：Builder/Processor 在构建时通过 CAS 获取并持有私有、
  不可克隆 lease；当前公开 builder 调用形态尽量保持，第二消费者返回 typed error。
* direct、callback 与 render 三个构建入口统一以
  `ProcessError::ConsumerAlreadyActive { processor: "Convolver" }` 报告 lease 冲突；
  `ConvolverProcessor::new` 与 offline render builder 直接切换为
  `Result<_, ProcessError>`，不保留 String/兼容构造路径。
* 任一构建后续步骤失败或 chain/processor 在非 RT 销毁时必须释放 lease；成功释放后
  同一 control 可重新构建一个 consumer。
* finish 期间观察到 disable 时延迟 retirement；剩余 tail 归零并进入 terminal 后，
  repeated finish 再推进 disabled ownership 到 control reclamation。

### P2 - state consistency and structure

* 内部控制决策不得读取 eventually-consistent `ConvolverStatus`；control 在发布 pointer
  后推进 `latest_published_generation`，disabled audio 仅在本地 ownership/finish 全空且
  published slot 为空后推进 `audio_drained_generation`。
* authoritative teardown 判定由 `ConvolverControl::is_quiescent()` 在 control gate 下
  检查 disabled、generation acknowledgement 相等及 published/retired slots 为空；
  `status()` 只负责观测，不再授权生命周期操作。
* 采用 targeted vertical module：`adapters/convolver.rs` 承载 RT adapter，
  `adapters/convolver/handoff.rs` 隔离 unsafe AtomicPtr ownership，
  `adapters/convolver/control.rs` 承载 control/status/lease/quiescence，
  `adapters/convolver/tests.rs` 承载相关测试；其余 adapter tests 外移到
  `adapters/tests.rs`，不重写其余七个 adapter 实现。
* 以一个私有 declarative stage manifest 作为 output-chain 编排源，生成 descriptor、
  callback 构造顺序以及 offline process/render/reset/sample-rate traversal；保留
  `OutputRenderChain` typed fields，不增加热路径分配或动态分派。
* Meter 从 signal/render stage metadata 中拆出，定义为显式 post-render analysis；
  基础 `OutputRenderChain` 不增加 Meter CPU、状态或结果字段。
* quality report 必须分别报告真实 render stage 与 post-render analysis，不得重复或
  虚构执行节点。

## Acceptance Criteria

* [x] 新音频线程首次 adoption、retirement、backpressure、recovery、finish 全程零分配/零重析构。
* [x] 并发 reclaim 与 audio hand-off 压力测试证明所有 kernel 只在 control/offline 线程析构。
* [x] RT hand-off 每个 block/finish 为固定次数原子操作，不依赖历史线程/debt-node 数量。
* [x] partial finish 后 disable 仍输出精确剩余 tail，并稳定终止为 `Finished(0)`。
* [x] 同一 control 的第二个 callback/render/direct consumer 被确定性拒绝。
* [x] 构建失败与 consumer drop 均释放 lease；释放前第二消费者失败，释放后重新构建成功。
* [x] publish-before/during-ack、disable/finish/retire/reclaim 交错均不会错误 quiesce；
  内部逻辑不调用 `status()`，stale acknowledgement 不能覆盖新 publication。
* [x] callback 与 offline stage metadata 和真实执行顺序一致；Meter 只出现在
  post-render analysis metadata/report 中，报告无重复。
* [x] 新增、删除或重排 stage 只需修改一个 manifest，callback/offline traversal 与
  descriptor/parity tests 随之同步；callback allocation gate 和性能基线不退化。
* [x] 双 feature tests、严格 Clippy、rustfmt、rustdoc、Convolver/callback/FIR/quality gates 通过。

## Research References

* [`research/findings-validation.md`](research/findings-validation.md) - 五项 finding 的代码与测试证据。
* [`research/rt-ownership-handoff-options.md`](research/rt-ownership-handoff-options.md) - AtomicPtr、SPSC ring 与 index pool 比较。
* [`research/output-stage-meter-options.md`](research/output-stage-meter-options.md) - Meter 作为 render stage 或 post-render analysis 的边界选择。
* [`research/output-stage-orchestration-options.md`](research/output-stage-orchestration-options.md) - canonical metadata 与真实 typed/dynamic traversal 的统一方式比较。
* [`research/telemetry-quiescence-options.md`](research/telemetry-quiescence-options.md) - authoritative quiescence 与 eventually-consistent telemetry 的分离方案。
* [`research/convolver-module-split-options.md`](research/convolver-module-split-options.md) - 在不重写其余 adapters 的前提下隔离 unsafe/control/RT state machine 的模块边界。

## Feasible Approaches

### Approach A - AtomicPtr ownership slots（推荐）

使用 published/retired 两个严格单向 ownership slot；control 分配和释放 Box，audio
只做固定 atomic exchange/CAS 与本地 staging。unsafe 被限制在独立小模块。

### Approach B - 预分配 SPSC ring

使用成熟或本地固定容量 ring 传递唯一 ownership；扩展性更强，但引入依赖/容量语义，
对当前单槽需求偏重。

### Approach C - index pool + state word

用预分配节点池和 index 状态机统一 ownership/telemetry；一致性最强但复杂度和内存
模型超出当前动态 IR 需求。

## Feasible Approaches: canonical stage orchestration

### Stage Option 1 - declarative manifest + generated traversals（推荐）

以一个私有 stage manifest 定义顺序、执行域和 field binding，并生成 descriptor、callback
构造顺序、offline process/render/reset/sample-rate traversal。保留 `OutputRenderChain` typed
fields，callback 维持现有 trait-object `DspChain`，不增加热路径分配或动态分派。

### Stage Option 2 - uniform trait-object container

offline 也改成有序 `Vec<Box<dyn StreamingProcessor>>`，让容器直接成为执行顺序；结构直观，
但 limiter/convolver 的类型特定访问需要 downcast、外置 handle 或扩大 trait，resampler 与
quantize 仍需特殊契约，并给 offline 增加不必要的 type erasure。

### Stage Option 3 - handwritten order + stronger tests

保留所有手写 traversal，只增加 snapshot/parity/trace tests。改动最小，但 metadata 仍不是
执行源，只能捕获已知漂移，不能根治多列表维护问题。

## Feasible Approaches: telemetry and quiescence

### Telemetry Option 1 - generation + drained acknowledgement（推荐）

control 发布 pointer 后推进 `latest_published_generation`；disabled audio 仅在本地
ownership/finish 状态全部为空且 published slot 为空后，写入
`audio_drained_generation`。authoritative `ConvolverControl::is_quiescent()` 在 control
gate 下检查 generation 相等及 published/retired 两槽为空；`status()` 只做观测。

### Telemetry Option 2 - packed epoch/state word

以一个 `AtomicU64` 编码 epoch 与 idle/backpressure/finishing flags，control CAS 推进
epoch，audio 仅在 epoch 未变时 CAS 提交状态。线性化更集中，但 bit/state transition
复杂，且仍需另查两个 pointer slots。

### Telemetry Option 3 - audio-owned enum + slot inspection

audio 单写 `Active/Finishing/Backpressured/Idle` enum，control 直接检查 enum 与 slots。
字段少，但 pointer 从 shared slot 移入 audio-local ownership 的跨原子窗口更难证明，
也不能说明 Idle 对应哪个 publication generation。

## Feasible Approaches: module split

### Module Option 1 - targeted Convolver vertical module（推荐）

保留 `adapters.rs` 的 shared helpers 与其余七个 adapter；新增私有
`adapters/convolver.rs`，再把 AtomicPtr unsafe primitive、control/lease/quiescence 与
Convolver tests 分别放入 `adapters/convolver/{handoff,control,tests}.rs`。其余 adapter
tests 机械外移到 `adapters/tests.rs`；现有公开类型从 `adapters` 根正常 re-export。

### Module Option 2 - only extract unsafe handoff

只提取 AtomicPtr helper，control、processor 与测试仍留在 `adapters.rs`。diff 最小，
但新 lease/finish/telemetry 会继续堆入 god module，不能完整解决结构 finding。

### Module Option 3 - split every adapter

八个 adapter 全部目录化并各自拆文件。长期最整齐，但引入大量与 P0 无关的机械变更和
visibility/test 风险，超出本任务边界。

## Decision (ADR-lite): RT ownership hand-off

**Context**：ArcSwap 的 DefaultStrategy 会在音频线程首次使用时分配 TLS debt node，
writer 还遍历全局只增不删的 debt 链；guard/reclaim 并发也可能把最后一次 kernel
析构转移到音频线程。当前业务只需要 current/latest 单槽语义，不需要有序多 kernel
队列。

**Decision**：采用 Approach A。新增独立、可审计的双向 ownership-slot 模块：control
把 `Box<PublishedConvolver>` 发布到 `published: AtomicPtr`，audio 用固定一次 exchange
取得唯一 ownership；audio 只用 CAS-from-null 向 `retired: AtomicPtr` 交还，失败时
继续持有固定本地 `pending_retire`；control 取回并完成所有 `Box::from_raw`/drop。
control-side publication 继续用现有 mutex 串行化并在 control 线程 latest-wins 释放
未被 audio 取走的旧值。

**Consequences**：RT hand-off 为严格 O(1)，不再依赖 Arc/Guard/debt-node，也不新增
依赖或扩大 heavy-kernel 容量。代价是引入一小段局部 unsafe，必须证明 raw pointer
恰好一次转回 Box、null-only CAS 不受 ABA 影响、shutdown 清空两槽且 processor/control
只在非 RT 销毁。SPSC ring 因 latest-wins 不匹配而拒绝；index pool 因状态空间和
动态 IR 内存模型过重而拒绝。未来有序 crossfade 另立任务重新评估 ring/pool。

## Decision (ADR-lite): consumer lease and finish boundary

**Context**：cloneable control/builder 可构建多个 live audio consumers，破坏 published
和 retired 的单消费者/单生产者不变量。另一方面，partial finish 保存非零 remaining
后立即 disable 会先退休 active kernel，导致后续 finish 返回 Backend error。

**Decision**：采用内部 lease 方案。`ConvolverControlInner` 以 CAS 维护一个 active
consumer；Builder 和 direct Processor 在构建时内部获取私有、不可克隆 lease，第二个
live consumer 返回明确 typed error。调用方无需传递 public lease，也不存在把其他
control 的 lease 传错 builder 的可能。finish 第一次进入时锁定当前 kernel/generation
和剩余 tail；之后的 disable 只记录控制意图，不得移走该 kernel，直到 tail 完整结束。
terminal repeated finish 才继续 disabled retirement/quiescence。

**Consequences**：现有 builder API 形态大体保留，但 direct processor 构造变为
fallible，所有构建入口必须传播 consumer-in-use 错误。并发 build 由 CAS 确定一个
成功者；旧 chain/processor 在非 RT 销毁并释放 lease 后可重新构建。finish 不再能用
disable 取消已承诺 tail；需要立即中止的宿主只能在 finish 前选择 bypass/reset policy，
本任务不新增 mid-finish tail cancellation 模式。

## Decision (ADR-lite): Meter boundary

**Context**：`OUTPUT_STAGE_DESCRIPTORS` 当前把 Meter 标为 offline stage，但
`OutputRenderChain` 不持有或执行 `LoudnessMeter`；quality report 又在包含 Meter 的
canonical CSV 后追加 `LoudnessMeter true-peak analysis`，导致 metadata 与真实执行
不一致且报告重复。Meter 是分析节点而非信号变换节点。

**Decision**：采用方案 1。把 Meter 从 signal/render stage metadata 拆出，定义为显式
post-render analysis；`offline_render_stage_names()` 只描述 `OutputRenderChain` 实际
执行的变换节点，另以独立 metadata/report 字段描述后处理分析。基础
`OutputRenderChain` 不自动构建或运行 Meter。

**Consequences**：修复 metadata 真实性而不增加所有 offline render 的 CPU、内存和
状态，也不扩展 `RenderedOutput`。旧 stage-name/CSV 语义会直接切换，调用方需要迁移；
未来如需一体化测量，另行设计 opt-in `render_and_measure` API，而不是污染基础 render
contract。

## Decision (ADR-lite): canonical stage orchestration

**Context**：当前 descriptor、callback builder、offline pre-quantize/full render、reset
和 sample-rate 更新分别手写顺序。单靠 snapshot/parity tests 只能检测已覆盖的漂移；
把 offline 统一为 trait-object vector 又会丢失 limiter/convolver 的直接类型访问，并
增加不必要的 type erasure。

**Decision**：采用 Stage Option 1。建立一个局部、私有的 declarative stage manifest，
编码顺序、执行域、rate domain 与 typed field binding，并由它生成 metadata、callback
构造和 offline traversal。source-rate transforms、optional resampler rate boundary、
output-rate noise shaper、terminal quantize 与 post-render analysis 使用明确的不同角色，
不伪装成完全同质的 processor list。

**Consequences**：metadata 与实际执行共享单一源；callback 保留现有
`Vec<Box<dyn StreamingProcessor>>`，offline 保留 typed fields 和静态分派，因此不新增
热路径分配、virtual dispatch 或 downcast。代价是少量局部 macro complexity；manifest
必须保持小而可读，并用展开后的 stage IDs/order 测试覆盖 optional resampler、quantize
和 Meter 分界。统一 trait-object offline 容器与“只补测试”方案均不采用。

## Decision (ADR-lite): telemetry and authoritative quiescence

**Context**：`ConvolverStatus` 由多个独立原子计数拼接，明确只保证 eventually
consistent；当前 audio 却用该快照判断 pending publication 并写 `audio_idle`，允许旧
audio 判定覆盖 concurrent publish 的 busy 写入。packed flags 可以阻止 stale CAS，
但仍不能独立证明两个 ownership pointer slots 为空。

**Decision**：采用 Telemetry Option 1。control 先发布 ownership pointer，再 Release
推进 `latest_published_generation`；disabled audio 只有在 finish-locked kernel 与
`owned/incoming/pending_retire` 全空、且没有待取 publication 后，才 Release 写入
`audio_drained_generation`。新增 authoritative `ConvolverControl::is_quiescent()`，在
control-only gate 下检查 disabled、drained generation 等于 latest generation、published
slot 为空且 retired slot 已由 control 排空。`ConvolverStatus` 继续提供 allocation-free
诊断快照，但移除其作为 teardown authority 的职责。

**Consequences**：两个 generation 各有一个逻辑写者，旧 acknowledgement 只能落后而
不能覆盖新发布；audio 不需要 CAS loop 或 packed-bit 状态机，成本仍为固定原子操作。
公开 shutdown 调用从 `status().is_quiescent()` 直接迁移到
`control.is_quiescent()`；必须继续先停止 publishers，避免 check 返回后又出现新发布。
generation 使用 wrapping u64，并以“不可存在 `2^64` 个未确认 publication”为契约；
barrier tests 覆盖 publish-before/during-ack、pointer withdrawal、finish tail、reclaim 与
wrap-adjacent 比较。

## Decision (ADR-lite): Convolver module boundary

**Context**：`adapters.rs` 当前 2,926 行，混合 shared helpers、八个 adapter、约 580 行
Convolver control/state machine 和约 1,338 行 tests。新方案还要加入局部 unsafe、lease、
finish locking 与 generation acknowledgement；只抽一个 helper 会继续把高风险协议埋在
god module 中，而拆分全部八个 adapter 会扩大无关 churn。

**Decision**：采用 Module Option 1。保留 `adapters.rs` 的 shared helpers 与其余七个
adapter；新增私有 Convolver vertical module，其中 `handoff.rs` 只负责 AtomicPtr/Box
唯一 ownership，`control.rs` 负责 publisher、status、consumer lease 与 authoritative
quiescence，module root 负责 `ConvolverProcessor` audio state machine，`tests.rs` 覆盖
其私有协议。现有非 Convolver tests 机械外移到 `adapters/tests.rs`。既有类型继续从
`processor::adapters` 正常 re-export，不公开内部模块。

**Consequences**：unsafe safety invariants、control policy 与 RT state transitions 可分别
审查；`adapters.rs` 回到 shared adapter root 职责，且不重写其他 adapter。代价是一次
中等规模文件移动和更严格的 private/`pub(super)` visibility 管理；测试辅助不得为了
方便而扩大 production public surface。目录结构 spec 必须同步更新。

## Expansion Sweep

* Future evolution：保留 generation 与 lease 边界，未来 crossfade/morphing 另立任务；
  本次不预留多 kernel 音频队列容量。
* Related scenarios：direct、callback、offline、terminal shutdown 必须共享相同 ownership
  与 finish 语义；benchmark/report stage 名称必须来自真实路径。
* Failure/edges：publisher burst、控制线程暂停 reclaim、disable during partial finish、
  consumer drop 顺序、构建第二消费者、generation wrap 与错误路径清理均需覆盖。

## Technical Approach

* 先做行为不变的 Convolver/test 文件拆分，再在独立 `handoff` 模块用两个
  `AtomicPtr<PublishedConvolver>` 实现 published/retired unique-ownership slots；只有
  control/offline 代码可执行 `Box::from_raw` 和 kernel drop。
* `ConvolverControlInner` 保留 control-only mutex 以串行化 cloned publishers，并用 CAS
  获取一个私有 consumer lease。三个构建入口统一传播
  `ProcessError::ConsumerAlreadyActive`；错误 unwind/drop 路径自动释放 lease。
* audio 每个 boundary 只执行固定次数 exchange/CAS/load/store，继续使用
  `owned/incoming/pending_retire` 固定 staging。control latest-wins 替换未取 publication，
  retirement 满时继续处理 current kernel并报告有界 backpressure。
* finish 第一次调用捕获并锁定 current kernel/generation 和精确 `IR length - 1` remaining；
  mid-finish disable 只记录意图，tail 归零后 terminal repeated finish 才推进 retirement。
* `latest_published_generation` 与 `audio_drained_generation` 构成 versioned acknowledgement；
  `ConvolverControl::is_quiescent()` 是唯一 authoritative teardown check，`status()` 不参与
  内部决策。
* output chain 使用私有 declarative manifest 生成 descriptor、callback construction、
  offline process/render/reset/sample-rate traversal；Meter 从 render stages 拆为独立
  post-render analysis metadata，并修正 benchmark/quality report。

## Implementation Plan

1. **结构基线**：机械提取 Convolver vertical module 与两类 tests，保持所有现有行为和
   public re-export 不变，先跑 focused tests 证明移动无回归。
2. **P0 ownership + lease + quiescence**：实现 AtomicPtr slots、single-consumer CAS lease、
   typed 三入口错误和 generation acknowledgement；加入新线程首次使用、并发 hand-off、
   destructor-thread、backpressure/recovery、双消费者和 stale-ack regression tests。
3. **finish/disable lifecycle**：实现 kernel/generation tail locking 和 terminal retirement，
   加入小 buffer 多次 finish、mid-finish disable、repeated terminal finish 与 reset tests。
4. **canonical output stages**：引入 declarative manifest，拆分 Meter post-analysis metadata，
   更新 exports、quality/callback benches 和真实 traversal parity tests。
5. **质量与契约**：运行 feature matrix、strict Clippy、rustfmt、rustdoc、相关 quick perf /
   quality gates；更新 realtime、streaming、directory-structure 与 quality specs。

每个批次保持可编译、可测试；不建立 ArcSwap/AtomicPtr 双路径或旧 API 兼容层。若 P0
集成必须回滚，应整体回滚 ownership 批次，不能留下混合 ownership protocol。

## Definition of Done

* 所有 acceptance criteria 有旧实现可失败的新回归测试并通过修复后验证。
* unsafe ownership 代码有逐项 safety invariant 文档、对抗性并发测试，并在本机
  toolchain 可用时运行 targeted Miri；不可用时明确记录。
* 项目完整质量门禁与相关性能基线通过，无未经批准的 public API 兼容层。
* realtime/streaming/output-chain specs 与任务研究同步，工作提交、归档和 journal 完成。

## Out of Scope

* 修改 FFT/partitioned convolution 数学算法、IR 响应、routing threshold 或音质调音。
* kernel crossfade、IR morphing、异步文件加载或核心库隐式后台 GC 线程。
* 让基础 `OutputRenderChain` 自动执行 Meter，或新增 `render_and_measure` 产品 API。
* 把 offline output chain 改为统一 trait-object 容器或扩大 `StreamingProcessor` 以容纳
  metadata/quantize/Meter。
* 引入 packed lifecycle state word、无版本 audio state enum 或通用 epoch reclamation。
* 把所有 processor adapter 全面重写成新框架；仅拆分本次 ownership 边界和必要测试。

## Technical Notes

* 主要文件：`src/processor/adapters.rs`、`src/processor/output_chain.rs`、exports、
  benches/tests 与 realtime/streaming specs。
* 当前任务目录 slug 保留首次创建值；task title/priority/scope 已扩展为五项总任务。
* 复杂度：Complex / architectural；先完成 P0 hand-off 决策，再依赖性顺序处理 P1/P2。
