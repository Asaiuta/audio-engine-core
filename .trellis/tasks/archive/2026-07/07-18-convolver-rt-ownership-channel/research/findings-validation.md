# Findings Validation

## Scope

2026-07-18 对当前 `main` 的 Convolver 控制面、streaming finish、输出链构造和
`arc-swap 1.9.1` 锁定源码进行只读审查。现有 Convolver 测试组 17/17 通过，
但测试矩阵缺少新音频线程首次 ArcSwap 使用、并发 reclaim/load、部分 finish 后
禁用和共享 control 的双消费者场景。

## Confirmed findings

1. **P0 - ArcSwap 不满足硬实时 ownership hand-off。**
   `try_flush_retired` 在音频线程调用 `retired.load/store`，`sync_convolver` 调用
   `published.swap(None)`。ArcSwap 的 DefaultStrategy 首次在线程取得 LocalNode
   时执行 `Box::leak(Box::default())`；每次 writer 还会遍历全局、只增不删的
   debt-node 链。并发 `load` guard 与控制端 `swap(None)` 可让 guard 在音频线程
   成为最后一个强引用并析构 kernel。
2. **P1 - partial finish 后禁用会返回稳定 Backend error。**
   已保存的 `finish_remaining_frames` 不会因禁用清除；disabled `sync_convolver`
   先退休 `owned`，随后 finish 在 remaining 非零时找不到 active kernel。
3. **P1 - single audio consumer 仅为文档约束。**
   `ConvolverControl`、`OutputChainParams`、`OutputChainBuilder` 均可克隆，build API
   接受 `&self`。两个消费者会争抢 publication，并可能并发覆盖 retired slot，
   在后写音频线程析构前一个 kernel。
4. **P2 - telemetry 存在 lost update。**
   disabled 音频路径读取 eventually-consistent `status().pending_kernels` 决定
   `audio_idle`；并发 publish 的 `audio_idle=false` 可被旧快照覆盖。完整
   `is_quiescent` 仍检查 pending/reclamation/backpressure 且要求 publisher 已停止，
   因此尚未证明会错误 quiesce，但内部控制不应依赖观测快照。
5. **P2 - 模块和 canonical stage 编排存在结构漂移。**
   `adapters.rs` 为 2,926 行，含 8 个 adapter 与约 1,300 行测试。output-chain
   descriptor、构造、process、render、reset、sample-rate 顺序分别手写。Meter
   被标记为 offline stage，但 `OutputRenderChain` 不执行它；quality bench 在 render
   后另做 true-peak 分析，因此这是 metadata/reporting 不一致，不是漏掉信号变换。

## Existing tests that mask the P0 issue

* no-allocation swap 测试先在同一测试线程调用 control-side `publish`，ArcSwap TLS
  已在断言外预热。
* destructor-thread 测试等待 audio process 完成后才在控制线程 reclaim，没有让
  `retired.load()` guard 与 `swap(None)` 并发。
* 当前没有对 RT 操作次数与历史线程数无关的有界性测试。

## Primary source locations

* `src/processor/adapters.rs`: Convolver control/status/processor and tests.
* `src/processor/output_chain.rs`: cloneable builder, duplicated stage orchestration, Meter metadata.
* `arc-swap 1.9.1/src/strategy/hybrid.rs`: writer `wait_for_readers`.
* `arc-swap 1.9.1/src/debt/list.rs`: global node traversal and first-use Box allocation.
