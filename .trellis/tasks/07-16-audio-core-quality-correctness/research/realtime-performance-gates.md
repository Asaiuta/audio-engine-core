# 实时性能门禁研究

## 研究问题

如何判断修复是否保持或提高实时性能，同时避免用机器相关的单次计时宣称“最高性能”？

## 当前基准事实

* 项目有六个 custom-main bench：callback chain、streaming resampler、quality measurements、convolver、lock-free params、FIR EQ。
* Callback、resampler、FIR 等部分基准以 best-of-N 报告；best 值适合看理想吞吐，却可能隐藏常见抖动或回退。
* Convolver 使用 trial median 并含同进程相对路径门禁；lock-free benchmark 也以相对改进为门禁。这两种方式比跨机器绝对 ns 阈值稳健。
* Callback `--enforce` 目前主要检查计时为 finite/positive，没有性能回退阈值。
* 当前机器 512-frame active DSP 约 `143.4 µs`，加入 256-tap convolver 约 `154.4 µs`；48 kHz 下回调周期约 `10.667 ms`，但这只是当前机器/构建条件的证据。
* `assert_no_alloc` 已是 dev dependency，项目规范要求预热后的 hot path 禁止分配/释放。

## 可比较的成熟模式

### 1. Rust Performance Book

Rust Performance Book 建议用相同工作负载比较两个实现，优先真实输入并辅以 microbenchmark/stress test，同时提醒内存布局等因素会造成显著而短暂的波动；Criterion、Divan、Hyperfine 或 custom harness 都是可行工具。

来源：https://nnethercote.github.io/perf-book/benchmarking.html

### 2. 同进程相对基准

本项目 convolver 与 lock-free benchmark 已有可复用模式：在同一进程、同一输入和同一机器上运行候选与参考路径，以比率设置宽松门禁。该模式消除了大量机器间差异。

来源：`benches/audio_convolver_perf.rs`、`benches/audio_lockfree_params_perf.rs` 及 archived benchmark inventory。

### 3. `assert_no_alloc`

该 crate 通过自定义 allocator 暂时禁止当前线程的分配/释放；其文档明确以音频线程不可预测的分配时延和 audible glitch 为使用动机。对实时安全而言，“零稳态分配”是比微小 ns 差异更硬、更可移植的门禁。

来源：https://docs.rs/assert_no_alloc/latest/assert_no_alloc/

## 建议的门禁层级

### 1. 实时安全不变量（硬门禁）

* 预热后 process、参数 snapshot、必要的 callback-facing drain 均零分配/释放。
* 静态/测试审核禁止 hot path lock、I/O、logging、panic 和不受 buffer/固定容量约束的循环。
* 多声道、最大合法 block、参数切换路径也需覆盖，而不只测默认 stereo steady state。

### 2. 结果有效性与覆盖（硬门禁）

* 每个场景必须实际产生/修改输出，并校验 frame count，避免“工作没做完所以更快”的假优化；本轮 resampler 正是该风险实例。
* Bench 输出记录 crate revision、profile、features、CPU/OS、sample rate、channels、buffer size、iterations、trials 和算法模式。

### 3. 同机相对回退（修复任务的主要性能门禁）

* 用相同二进制内的 reference path，或同一受控环境中的 before/after artifact 做配对比较。
* 至少报告 median；同时报告 p95/max 或完整 trials 作为抖动证据，不再只采用 best-of-N。
* 初始建议把 `>10%` median 回退作为调查阈值，而非自动证明失败；待收集多平台重复数据后再决定是否升级为硬 gate。
* 对具有天然 reference 的路径继续使用相对门禁，例如 inplace vs into、cached snapshot vs legacy、streaming vs one-shot。

### 4. Callback deadline utilization（容量证据）

* 报告 `buffer_duration`、`median/p95 callback time` 与 deadline utilization，而不只报告 ns/sample。
* 64/128/256/512/2048 frame，active DSP ± convolver，以及参数变更后的首个 buffer 都应覆盖。
* 没有固定 runner 前，absolute utilization 保持 report-only；受控 runner 建立稳定分布后才设置硬预算。

### 5. 算法优化证据

* 优化前先证明输出等价或质量不低于明确阈值，再比较性能。
* 控制线程成本（FIR/IR 生成、kernel swap）和 audio thread 成本分开报告。
* 长时间 stress 测试应覆盖 convolver disposal、频繁参数更新及 reset/finalize，捕获短基准看不到的资源生命周期阻塞。

## 可行实施方式

### A. 演进现有 custom harness（推荐）

统一条件元数据、JSON 输出、median/p95、deadline utilization 和 before/after 对比；保留当前简单、无额外 runtime dependency 的基准入口。

优点：改动可控，能复用现有六个 bench；缺点：统计、基线存储和比较逻辑需自行维护。

### B. 引入 Criterion/Divan 处理 microbench

保留 quality custom harness，把纯性能 bench 迁移到统计框架。

优点：统计分析与回归比较成熟；缺点：迁移成本、CI 时间和输出整合成本更高，且 deadline/音频场景元数据仍需自定义。

### C. 仅保存人工基准报告

修复前后手工运行并在任务记录结果，不扩展门禁。

优点：最快；缺点：无法持续防止回退，不满足本次选择的“可持续性能门禁”目标。

## 对本任务的建议

* 选择 A；先标准化 callback/resampler 两个与 P0 直接相关的 bench，再按 P1 子任务扩展其他 bench。
* 第一阶段不设置跨机器绝对 ns 硬阈值；使用零分配硬门禁、结果完整性硬门禁和同机相对报告。
* 每个实现子任务在 PRD/研究记录中保存修复前后同条件结果；任何超过 10% 的 median 回退必须解释、优化或经用户明确接受。

