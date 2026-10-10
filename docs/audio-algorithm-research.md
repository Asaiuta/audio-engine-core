# 音频算法改进方向：公开论文与当前实现对照

调研日期：2026-10-10。源码基线：`c9cb522`，同时读取当前工作区中的已有研究记录。
本文记录调研时的文献筛选与实现对照；后续卷积实现及实测见
[非均匀卷积性能记录](convolver-performance.md)。调研本身没有执行新基准或修改算法。
允许改变实现及输出样本，但客观质量不能降低；不要求与 Rubato 逐帧一致。

## 优先级结论

| 优先级 | 模块与方法 | 当前实现 | 值得做的改进 | 尚不能承诺的部分 |
| --- | --- | --- | --- | --- |
| 第一：性能 | 长 IR 非均匀分区卷积 | 长 IR 使用 1024 帧均匀分区，已有 head/tail 和分散 MAC 调度 | 保留完整 IR，后段改用更大、执行频率更低的分区 | 本项目加速比例、内存降低、回调最坏耗时均待实现后确认 |
| 第一：质量 | 饱和器 ADAA，优先简单 ADAA + 2× | 线性插值升采样、非线性整形、对 nonlinear delta 做 FIR 降采样 | 在非线性生成谐波时抑制混叠，可能降低高倍过采样需求 | 会影响频响、延迟和瞬态；不能自动视为比当前 4× 更快且质量全面更高 |
| 第二：功能质量 | pYIN 时序解码 | YIN 与 pYIN 第一阶段候选，明确没有 HMM/Viterbi | 连续基频及有声/无声跟踪，减少孤立候选跳变 | 需要真实录音证据；完整序列解码与低延迟流式解码有不同语义 |
| 有条件研究 | CoreSinc 完整多级设计、任意比率插值 | FFT、halfband、sinc/polyphase 混合路由 | 针对明确瓶颈重新分配各级误差、运算和存储预算 | 既有等波纹与多级研究不能重复当成新方案；没有达到 FFmpeg 默认速度的质量等价证据 |
| 观察 | PolyADAA；Beat This + CASM | 尚未集成这些方法 | 前者研究更强抗混叠；后者研究节拍序列一致性 | 较高算力、模型/数据依赖；CASM 目前是预印本 |

## 1. 长卷积：最接近保质提速的结构性机会

[当前 convolver](../src/processor/convolver.rs) 在 IR 超过 4096 帧时使用
1024 帧均匀分区。尾部分区的乘加已有分散调度，所以“引入 FFT 分区”或
“把乘加摊到多个回调”本身不构成新改进。

Battenberg 与 Avižienis 的 DAFx 2011 论文 [1] 第 2.2 节把非均匀分区
解释为不同块长的多个频域延迟线：靠近 IR 起点使用小块，较晚的 IR 使用
大块。晚到的响应提供额外计算时间，因此可以降低后段的执行频率，同时
保留早段的响应时间。每种尺寸可以保留多个分区，不必机械地每块翻倍。

对本项目可先只增加一个较大尺寸的尾部级，保留现有头部与调用接口。
这改变的是卷积求和的组织，在实数数学中仍然实现同一个完整 IR；浮点加法
顺序可能变化，无需要求逐样本位一致。

论文同时指出调度截止时间的重要性。不能把大 FFT 集中到偶发回调后仅凭
平均 CPU 降低宣布成功，也不能直接照搬会等待工作线程的实现。初始方案
优先沿用项目现有线程边界；是否需要拆分 FFT 工作应由较大级的截止时间决定。
内存仍受 IR 频谱和输入历史支配，不能从计算次数减少推导内存同比减少。

2025 年 MCFX-6DoFconv 论文 [2] 的摘要报告相对原 SPARTA 6DoFconv 最高
3.7 倍效率改善。这是不同软件和空间音频场景中的结果，不能当作本项目
预期倍率。它支持该方向仍有工程价值，而不是提供本项目收益证据。

## 2. 饱和器：ADAA 是明确不同于当前实现的方法

[当前 saturation](../src/processor/saturation.rs) 提供 Direct、2×、4×。
过采样路径先线性插值，再执行包含 threshold、drive、软膝的完整整形函数，
把 `shaped - interpolated` 写入 FIR 历史。2×/4× 分别使用 17/33 taps；
启用路径已有固定 4 个源帧延迟及状态切换处理。

ADAA 的核心是对采样点之间的非线性输出求平均，而非只在离散点计算整形
后再滤波。以一阶矩形核为例，若 `F'(x) = f(x)`：

```text
y[n] = (F(x[n]) - F(x[n-1])) / (x[n] - x[n-1])
```

相邻输入接近时需要稳定的极限分支。2016 年原始方法 [3] 及后续研究支持
ADAA 与低倍过采样组合；这使“简单 ADAA + 2× 对比当前 4×”成为值得落地
的单一候选，而非直接增加到 8×/16×。

质量约束不能省略：

- 积分必须覆盖完整的阈值、软膝和整形函数，不能只套用 `tanh` 的原函数。
- 对 `f(x)=x`，上述公式输出相邻两点的平均，说明它自带高频衰减和半采样
  延迟；更少混叠不等于所有频点更准确。
- 当前实现保留 dry、只过滤非线性差值。可研究对 `g(x)=f(x)-x` 应用
  ADAA，以保留这条设计思路，但差值路径的延迟、频响和 dry/wet 对齐仍需处理。
- 动态参数下，两个原函数值必须使用一致的当前曲线定义；参数切换、近零
  分母及尾部处理需要与现有状态协议相容。

2025 年 LUT 积分论文 [4] 给出了难以求解析原函数时的工程方案，但引入了
插值、积分误差与表内存。它是备选工具，不是无需代价的加速结论。

2026 年 PolyADAA [5] 把输入重建提升为高阶 Lagrange 插值，并以 Chebyshev
近似计算积分。可读取的第 3.4–3.5 节明确给出：`N=3, K=16` 的示例每个
输出样本需要 17 次非线性函数求值、约 119 次插值浮点运算和长度 32 的 FFT，
另有最终加权求和。它避开传统差商的近零分母问题，但计算代价明显，暂列
高质量研究方向，不作为优先性能优化。本文没有据其图表推出本项目音质收益。

## 3. 重采样：新论文不能抹掉已有否定证据

CoreSinc 已是混合架构：常规线性相位转换使用 FFT overlap-add，特定 2×
High 使用 halfband，特殊大整数比走输出驱动 sinc；非线性相位另有频域及
连续多相实现。具体路由见 [后端](../src/processor/resampler/rust_backend/mod.rs)
及 [分块实现](../src/processor/resampler/rust_backend/block.rs)。

Filip 2016 的稳健 Parks–McClellan 算法 [6] 解决的是滤波器设计求解器的
数值困难；更稳健地得到 minimax 系数，并不等于运行时卷积免费变短。
本项目此前的 R3 完整替换研究记录（保留在本机开发工作区，不随仓库分发）
已经包含 88 个相关设计尝试、72 个有限系数且完成流式评估的设计，其中
3 个通过对应开发门槛但仍有逐频点或瞬态取舍。多级、halfband、CIC 加补偿
的那组开发实验为 **0/72**。这些分母是具体候选配置，不是对整个方法族的否定。

本日短多相初筛的研究记录（保留在本机开发工作区，不随仓库分发；机读结果见
[多相筛选数据](data/resampler-polyphase-screen-2026-10-10.json)）
也说明：在该研究设定的近阻带 −180 dB 筛选条件下，普通转换需要约 96
taps/phase，96→44.1 kHz 需要 192；不能把 FFmpeg 的短核工作量直接移植
过来并宣称保留现有响应。这不是所有未来设计的理论下界。

真正不同的新尝试必须先说明如何降低完整转换的总成本：在哪个采样率
执行各级滤波、每一级阻带误差折叠到哪里、如何覆盖流式边界，以及相位表
和插值的成本。只改变窗函数、继续缩短 taps 或扩大旧参数网格，不足以
支持再次大规模实验。Farrow [7] 更适合可变分数延迟/任意比率需求，仍需
处理降采样抗混叠，不能直接视为常用固定比率 FFT 路径的性能升级。

截至本轮调研，结论仍是：**存在研究空间，但没有发现已证实能在本项目
当前客观质量下稳定达到 FFmpeg 默认速度的替代方案。** 当前实测边界见
[质量与性能对比](resampler/comparison.md)，论文方法不替代这里的实测数据。

## 4. 音高与节拍：区分缺失功能和新研究

[PitchAnalyzer](../src/analysis/pitch.rs) 的注释明确声明：已有 YIN steps
1–5 与 pYIN 阈值候选，尚无 HMM/Viterbi，候选概率也不是校准后的置信度。
pYIN 论文 [8] 的时序解码是可补齐的具体能力；公开 librosa 实现也明确将
Viterbi 作为第二阶段。序列跟踪可以抑制孤立跳频，但可能平滑真实滑音或
音符起点；需要区分离线完整序列和有界延迟流式模式，放在分析工作线程。
它是音高分析质量机会，不是音频回调提速方案。

[AutoMix tempo](../crates/audio-automix/src/tempo.rs) 当前为恒定速度假设下的
Ellis 风格 DP。Beat This [9] 用卷积与时频 Transformer 生成节拍激活，
其摘要明确报告 F1 优势与连续性指标劣势并存。

2026-10-07 提交的 CASM [10] 是新的观察对象：针对神经节拍激活使用
上下文相关的半马尔可夫解码，显式处理半速/倍速歧义。正文第 3.3 节说明
参数经过开发集网格校准，并非“无学习参数”就无需调参；论文使用固定
神经激活，在 GTZAN/SMC 上报告连续性改善。目前是投稿 ICASSP 2027 的
预印本，不能称为已录用成果，更不能直接接到当前 onset 曲线上推定有效。
不因此重启已冻结的 AutoMix 研究或改变既有验收语料。

## 5. 暂不优先的替换

- **EQ 全面改为 TPT/SVF**：当前 10 段 peaking biquad 已有旧/新状态交叉
  淡化。频繁连续调制可能有不同需求，但本轮没有找到足以证明替换静态 EQ
  更快且更好的项目证据；保留当前实现。
- **“新增 true-peak limiter”**：项目默认已采用 4× FIR true-peak 检测，
  且输出链有最终浮点域 lookahead limiter。ITU-R BS.1770-5 [11] 是响度
  和 true-peak 测量标准，不是更优限幅包络的证明。后续若有明确失真或
  ceiling 问题，再针对该问题研究。
- **混响低秩近似、丢频点/相位、裁剪 IR**：可能有明显加速，但会改变目标
  响应。DAFx 2020 的卷积优化论文 [12] 本身就报告轻微质量下降，不符合
  本轮“客观质量不能降低”的默认替换条件。

## 6. 后续工作边界

若进入实现，建议先做一个长 IR 的非均匀尾部分区候选，或一个简单
ADAA + 2× 候选；不要同时开展多条大规模搜索。先复用现有输入与基线做
对应模块的小范围质量筛选，质量失败即停；通过后才测该模块的 CPU 时间、
回调耗时尾部、保留/峰值内存和必要延迟。没有新增问题就不重复扩大验证。
设备调度与端到端播放延迟仍需消费应用参与，不由纯 DSP 库测试替代。

## 来源与实际阅读范围

“摘要”代表未据此声称读完全文；获取失败不记为已验证全文。

1. Eric Battenberg, Rimas Avižienis. **Implementing Real-Time Partitioned Convolution Algorithms on Conventional Operating Systems**, DAFx 2011. [公开 PDF](https://www.dafx.de/paper-archive/2011/Papers/90_e.pdf)。取得完整 8 页 PDF；读取算法、分区与调度相关正文。
2. Domenico Stefani et al. **Improved Real-Time Six-Degrees-of-Freedom Dynamic Auralization Through Nonuniformly Partitioned Convolution**, 2025. [DOI](https://doi.org/10.17743/jaes.2022.0224)。Crossref 元数据与摘要；年份依据发布日期，不依据 DOI 中的 2022。
3. Julian D. Parker, Vadim Zavalishin, Emmanuel Le Bivic. **Reducing the Aliasing of Nonlinear Waveshaping Using Continuous-Time Convolution**, DAFx 2016. [公开 PDF](https://www.dafx.de/paper-archive/2016/dafxpapers/20-DAFx-16_paper_41-PN.pdf)。DAFx 官方摘要；方法背景同时见 [5] 可读正文。
4. Leonardo Gabrielli, Stefano Squartini. **Simplifying Antiderivative Antialiasing with Lookup Table Integration**, DAFx 2025. [公开 PDF](https://www.dafx.de/paper-archive/2025/DAFx25_paper_30.pdf)。DAFx 官方摘要。
5. Leonardo Gabrielli, Stefano Squartini. **PolyADAA: Improving Aliasing Reduction in Memoryless Nonlinearities Using Lagrange Interpolation and Polynomial Approximation**, DAFx 2026. [公开 PDF](https://www.dafx.de/paper-archive/2026/papers/DAFx26_paper_47.pdf)。官方摘要及部分正文：下载未完整，MuPDF 恢复了包括 3.4–3.5 节的文本；不据缺失页面、图形或残缺表格作结论。
6. Silviu-Ioan Filip. **A Robust and Scalable Implementation of the Parks-McClellan Algorithm for Designing FIR Filters**, 2016. [DOI](https://doi.org/10.1145/2904902)。Crossref 元数据与摘要。
7. C. W. Farrow. **A Continuously Variable Digital Delay Element**, 1988. [DOI](https://doi.org/10.1109/ISCAS.1988.15483)。核对书目信息；本文的应用取舍是算法层面判断，不是复现论文结果。
8. Matthias Mauch, Simon Dixon. **pYIN: A Fundamental Frequency Estimator Using Probabilistic Threshold Distributions**, ICASSP 2014. [DOI](https://doi.org/10.1109/ICASSP.2014.6853678)。核对书目信息，并读取 [librosa 公开实现说明](https://github.com/librosa/librosa/blob/main/librosa/core/pitch.py) 与本项目 stage-1 边界；未获取原论文全文。
9. Francesco Foscarin, Jan Schlüter, Gerhard Widmer. **Beat this! Accurate Beat Tracking without DBN Postprocessing**, ISMIR 2024. [arXiv](https://arxiv.org/abs/2407.21658)。作者摘要与录用信息。
10. Zhanhong He, Hanyu Meng, Yaolong Ju. **CASM: Context-Aware Semi-Markov Post-Processor for Beat Tracking**, 2026-10-07 预印本. [摘要](https://arxiv.org/abs/2610.09318)、[HTML 正文](https://arxiv.org/html/2610.09318v1)。读取方法、校准、评估协议及结论相关段落。
11. ITU-R **BS.1770-5 (11/2023)**. [官方标准页面](https://www.itu.int/rec/R-REC-BS.1770/en)。核对现行版本与范围。
12. **Optimization of Convolution Reverberation**, DAFx 2020. [公开 PDF](https://www.dafx.de/paper-archive/2020/proceedings/papers/DAFx2020_paper_1.pdf)。DAFx 官方摘要，包含丢弃频点/相位及听音质量下降的限制。
