# 客观音质与算法验证研究

## 研究问题

如何把“最佳音质/最佳算法”转化为不夸大、可重复、能真正捕获本轮数学与状态缺陷的工程门禁？

## 现有能力与盲区

* `audio_quality_measurements` 已具备 THD+N、频响、阻带混叠、oversampled saturation、EQ、crossfeed、dynamic loudness、limiter、noise shaping、响度及完整输出 true-peak 测量，并区分 `gate` / `report` / `skipped`。
* 合成指标阈值总体保守，适合跨 CPU/编译器；EBU 3341/3342 外部语料缺失时会标记 skipped，这是正确行为。
* 完整输出 true-peak 仍是 report-only；本轮约 `-0.610 dBTP`，不能据此宣称满足 `-1 dBTP` 最终输出保证。
* 部分门禁只证明当前行为稳定，不证明模型正确。例如现有 crossfeed 指标固化了“低频比高频更弱”的高通行为；若采用 Bauer 低频交叉馈送模型，必须先纠正测试 oracle，不能把旧阈值当规范。
* `LoudnessMeter` 与直接 `ebur128` 的零差异能证明 wrapper 传播正确，但二者共享同一实现时，不能独立证明算法符合标准；外部 expected-value corpus 仍不可替代。
* 现有指标未覆盖：输入消费完整性、末帧 tail、reset 隔离、EQ 目标状态交接、FIR 1-tap NaN、全频统一增益、saturation threshold 连续性等本轮缺陷机制。

## 可比较的标准与参考模式

### 1. ITU-R BS.1770-5

ITU 当前有效版本为 BS.1770-5（2023-11），定义节目响度和 true-peak 测量算法。适合作为响度/true-peak 的规范来源，而不是用本项目自己的输出反向定义正确值。

来源：https://www.itu.int/rec/R-REC-BS.1770/en

### 2. EBU Tech 3341/3342 测试序列

项目已经定义 expected-value 与容差并支持外部 corpus。正确策略是：存在时强制执行；缺失时明确 skipped 并让发布证据显示缺口，而非静默通过。

来源：现有 `benches/audio_quality_measurements.rs` expected-value 表及 archived quality-gates research。

### 3. W3C Audio EQ Cookbook / RBJ

该参考给出标准 biquad 原型、BLT 转换和 shelving 的 `alpha`/中间变量公式。Dynamic loudness shelf 应逐项对照公式，并使用解析频响点验证，而不能只验证输出 finite。

来源：https://www.w3.org/TR/audio-eq-cookbook/

### 4. 独立/朴素参考实现

适用于本仓库的常见 oracle：

* 短 IR 使用直接时域卷积对照 FFT/partitioned convolution。
* Biquad 使用解析传递函数频响对照实现输出。
* 一次性处理对照随机分块处理，验证 chunk invariance。
* 新旧滤波器各自持续运行的双路径 reference，对照 crossfade 后状态交接。
* impulse、step、silence、DC、Nyquist 邻域 tone 与确定性随机信号覆盖不同缺陷类别。

## 建议的分层门禁

### Layer 1：数据完整性与有限性（单元/性质测试，硬门禁）

* consumed/produced 总量、随机分块等价、drain 终止、reset 隔离。
* 所有公开合法配置输出 finite；非法配置显式拒绝或规范化，不产生 NaN。
* 末帧 impulse 与已声明 tail 不丢失。

### Layer 2：数学与状态 oracle（硬门禁）

* RBJ shelf 在代表性采样率/频率/增益下与解析参考系数或频响一致。
* EQ transition 对照“目标滤波器从切换开始持续积累状态”的 reference。
* FIR 统一 +6 dB 在通带保持约 +6 dB；1-tap 退化为明确定义的纯增益/单位 impulse。
* Saturation 在 threshold 左右的函数值连续，并按需要检查一阶斜率或最大跳变量。
* AutoMix 的 BPM 换算使用实测 hop rate，并用已知 BPM click fixtures 验证。

### Layer 3：客观音频指标（`--enforce` gate）

* 保留 THD+N、passband、alias、limiter ceiling、noise-shaping direction 等已有高余量指标。
* 新增 transition discontinuity、最终 tail preservation、full-output length/true-peak 等与本次修复直接相关的指标。
* 阈值先由标准、解析误差界或多平台观察值决定；没有依据的指标先 report，不随意变成硬 gate。

### Layer 4：外部 conformance corpus（存在时硬门禁）

* EBU vectors 等语料必须记录来源/版本/哈希；缺失时 skipped 计数在摘要中可见。
* 发布“符合标准”前，要求受控环境跑过 corpus；普通本地快速检查可允许 skipped。

### Layer 5：主观试听（证据补充，不作自动门禁）

* ABX/MUSHRA 可用于选择调音偏好，但不能替代数学正确性或数据完整性门禁。
* “最佳音质”应改写为具体、可测的保证或明确的偏好模式。

## 阈值原则

* 精确传播/共享参考：使用紧容差，但避免把同一底层实现误当独立验证。
* FFT、随机 dither、跨 CPU 浮点：给足余量或验证方向/相对关系。
* 发生过的缺陷探针必须转成回归阈值；阈值应能明确让旧实现失败。
* 报告必须包含测试条件、gate/report/skipped 分类和 measured-vs-threshold。

## 对本任务的建议

* 不建立一个笼统“音质得分”；按算法保证分别验证。
* P0 子任务以 Layer 1/2 为主，并把 full-output tail 纳入 Layer 3。
* P1 算法子任务逐项修正 oracle 后再更新相应质量门禁，特别是 crossfeed 与 dynamic loudness。
* 外部 EBU corpus 单独建立可获取性/许可/哈希方案；在完成前保留显式限制说明。

## 最终外部语料证据（2026-07-18）

用户提供的本地 `libebur128/test` 包含门禁需要的全部 EBU Tech 3341/3342
测试序列。`ebu-loudness-test-setv05.zip` 大小为 91,631,421 bytes，SHA-256
为 `9CC500B4DF83F7C21855C74DCE795EF5209A752BF884253AE57D0CE512EFB062`；
语料只作为本地验证输入，不随 crate 分发。

`audio_quality_measurements` 的 quick/full `--enforce` 均得到 25/25 gates、
4 个 report-only 指标、0 skipped。55 个响度测试点的最大全局/LRA/瞬时/短时
误差分别为 `0.029032`、`0.000432`、`0.006402`、`0.066260 LU`；9 个
true-peak 测试点的最大绝对误差为 `0.181438 dB`，全部处于既定 EBU 容差内。
因此本轮不再保留“外部 EBU corpus 缺失”的覆盖限制；完整输出 true-peak
约 `-0.610 dBTP` 仍是独立的 report-only 结果，不能扩张为通用输出上限或
笼统“最佳音质”结论。
