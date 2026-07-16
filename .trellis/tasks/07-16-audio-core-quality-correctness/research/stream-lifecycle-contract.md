# 流式 DSP 生命周期契约研究

## 研究问题

如何在不破坏实时回调约束的前提下，统一表达输入消费量、输出生产量、算法延迟、尾部、结束排空和状态重置，并避免本轮出现的 resampler 丢帧与离线 limiter 截尾问题？

## 当前实现事实

* `AudioProcessor` 只有原地 `process(&mut [f64], channels)` 与 `reset()`；它适合固定帧数、原地回调 DSP，但无法表达可变速率处理、部分输入消费或额外尾部输出。
* `OutputStageDescriptor` 只记录 `carries_state`、`introduces_latency` 和文字说明，运行时无法依据这些元数据查询具体帧数或排空阶段。
* `StreamingResampler` 当前只使用 SoXR 的 `output_frames`，未处理 `input_frames`；当输出 scratch 限制导致只消费部分输入时，剩余输入会静默丢失。
* Resampler 的 flush 以 `process(&[])` 模拟结束，reset 只清 Rust 侧缓冲；这与依赖库公开的 `drain()` / `clear()` 契约不一致。
* `OutputRenderChain::render` 只对输入执行一次处理，没有把 limiter look-ahead、卷积尾部和 resampler 尾部作为一个有序的结束阶段传播到下游。

## 可比较的成熟模式

### 1. SoXR Rust 0.6.0

本项目锁定的 crate 源码明确规定：

* `process` 返回 `Processed { input_frames, output_frames }`；调用方必须同时推进输入和输出游标。
* 流结束后调用 `drain(output)`，直到返回 0。
* `clear()` 调用原生 `soxr_clear`，用于清除内部状态。

结论：本轮 resampler 修复不需要推测，应严格实现依赖库已有契约。

来源：本机 Cargo registry 中 `soxr-0.6.0/src/lib.rs`；上游 crate API。

### 2. Rubato Resampler

Rubato 的流式接口采用以下组合：

* `process_into_buffer` 返回 `(input_frames, output_frames)`。
* 暴露 `input_frames_next/max`、`output_frames_next/max` 和 `output_delay()`。
* 最后一段可声明 partial input；以 0 个有效输入帧继续处理来排空延迟。
* `reset()` 的文档契约是清除所有内部缓冲。
* 分配型便捷接口与预分配的实时接口分离。

结论：消费/生产进度、容量查询、延迟查询及实时/便捷 API 分层是成熟且清晰的组合。

来源：https://github.com/HEnquist/rubato/blob/master/src/lib.rs

### 3. VST3 `IAudioProcessor`

VST3 将以下概念明确交给宿主：

* `getLatencySamples()` 报告 group delay/look-ahead 延迟。
* `getTailSamples()` 报告无尾部、有限尾部或无限尾部，供离线处理避免截断。
* setup、processing state 与 process 分开；文档强调处理阶段不做重配置或分配等重操作。

结论：延迟和尾部是宿主/渲染器必须理解的运行契约，而不应只存在于描述字符串中。

来源：https://github.com/steinbergmedia/vst3_pluginterfaces/blob/master/vst/ivstaudioprocessor.h

## 映射到本仓库的约束

* 实时 callback 的固定大小原地处理路径已经被广泛使用，不能为了离线 drain 让所有 processor 都改成动态 `Vec` 输出。
* Resampler 是可变速率、可部分消费的特殊阶段；其进度类型必须是一等返回值，不能只返回输出帧数。
* 离线结束不是“给整条链补一块静音”这么简单：上游产生的尾部必须继续流过所有下游阶段，最终再排空下游自己的尾部。
* drain/finalize 必须定义幂等性：完成后重复调用产生 0 帧；新的流开始前通过 reset 恢复初始状态。
* 所有 callback-facing drain/process_into 路径必须由调用方提供容量，且在预热后无分配。

## 可行方案

### A. 双层兼容契约（推荐）

保留现有 `AudioProcessor` 作为固定帧数、原地实时接口；新增独立的流式/结束能力类型：

* `ProcessProgress { input_frames, output_frames }` 用于 resampler 等可变 I/O 阶段。
* 统一的生命周期查询至少提供 `latency_frames()`、`tail_frames()` 或有限/无限尾部枚举。
* 可排空阶段提供 caller-owned buffer 的 `drain_into`/`finish_into`，完成后返回 0。
* `OutputRenderChain` 负责按阶段顺序传播尾部，不把复杂度塞进实时 callback trait。
* 旧便捷 API 保留，内部改为驱动新契约，避免静默行为变化。

优点：兼容现有实时架构；能直接解决 P0；职责清楚。缺点：存在两类 trait/API，需要文档说明哪类阶段使用哪一种。

### B. 给 `AudioProcessor` 增加默认生命周期方法

在现有 trait 上添加默认的零延迟、零尾部、空 drain 方法。

优点：统一发现能力、迁移量较小。缺点：现有 `process` 仍不能表达部分消费和输出扩张；容易给人“所有阶段已经统一”的错觉，resampler 仍需旁路接口。

### C. 统一为可变 I/O streaming trait（破坏性重构）

所有处理器都改成 input/output buffer + consumed/produced + finish/reset 的统一接口。

优点：抽象最整齐，离线图渲染最通用。缺点：公共 API 和全部 adapters/benches/tests 大范围迁移；固定原地 callback 路径变复杂，当前 P0 修复不需要承担这项风险。

## 建议的契约测试

* `process(all-at-once) == process(random chunks) + drain`，允许算法定义内的浮点容差。
* 所有输入帧只消费一次；消费总量必须等于完整输入帧数。
* drain 最终终止，完成后重复 drain 输出 0。
* reset 前喂入非零信号，reset 后处理零信号，不得出现前一流残留。
* impulse 位于最后一帧时，最终输出仍包含 impulse 及定义内尾部。
* 延迟/尾部查询和实测 impulse 位置、最终非零帧范围一致。
* 预分配后，callback-facing process/drain 路径通过 `assert_no_alloc`。

## 用户决策后的统一 trait 设计补充

用户明确选择了前述方案 C（破坏性统一 streaming trait），并要求直接移除旧 API；buffer 范围限定为 interleaved `f64`，不在本轮泛化 `f32` 或 planar layout。基于本仓库的动态 `DspChain` 与实时要求，建议把统一语义和原地优化同时放进一个对象安全契约：

* 使用只借用 slice 的 `AudioBlockRef` / `AudioBlockMut`，在构造时验证 `samples.len() % channels == 0`，不分配、不复制。
* 使用安全枚举表示 `InPlace(AudioBlockMut)` 与 `OutOfPlace { input, output }`，避免为同一内存同时制造 Rust 共享/可变借用。
* 固定 1:1 processor 在 in-place arm 维持当前热路径；resampler 等可变 I/O processor 使用 out-of-place arm。枚举分派发生在每块一次，不进入逐样本循环。
* 两种 arm 都返回相同的 consumed/produced/status，因而属于同一个 streaming 生命周期，而不是两套 processor trait。
* `NeedOutput` 是正常 backpressure；非空输入/输出容量下连续零进度属于契约错误，chain 必须停止并报告，不能死循环。
* `finish` 重复调用必须最终并持续返回 finished/0；`reset` 同时清除 Rust 与 native backend 状态。
* Chain 为可变 I/O 阶段预分配 scratch/ping-pong buffer；固定阶段继续原地处理，避免为了抽象统一强制所有 stage 增加一遍内存写流量。

该设计保留统一生命周期的长期收益，同时避免“always out-of-place”对当前 512-frame callback 链造成不必要的内存带宽回退。
