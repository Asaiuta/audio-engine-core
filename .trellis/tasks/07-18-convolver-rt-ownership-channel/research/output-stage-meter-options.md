# Output Stage And Meter Boundary Options

## Current mismatch

`OUTPUT_STAGE_DESCRIPTORS` 把 Meter 标为 `offline_stage=true`，因此公开的
`offline_stage_names()`/CSV 声称 canonical offline chain 以 Meter 结束。但
`OutputRenderChain` 只有 volume、EQ、saturation、crossfeed、convolver、dynamic
loudness、limiter、optional resampler、noise shaper 和显式 quantize；它不持有或执行
`LoudnessMeter`。quality bench 在 render 返回后另行测 true peak，同时又把已含
`Meter` 的 CSV 拼接到 `-> LoudnessMeter true-peak analysis` 前，形成重复语义。

Meter 不改变信号，所以这不是漏执行 DSP；它是 public metadata、报告和真实执行边界
不一致。

## Option A - Separate render and post-render analysis metadata（推荐）

把真实音频执行节点定义为 render/transform stages；Meter 移到单独的
post-render-analysis descriptor/API。`offline_render_stage_names()` 只报告实际执行的
volume 到 quantize，quality bench 再显式报告 `post_render_analysis=...`。如需保留总览，
可以有一个组合 report plan，但不得称其为 `OutputRenderChain` 的执行顺序。

优点：与当前运行行为一致，不增加 render CPU/内存或 `RenderedOutput` API；明确区分
信号变换和测量。缺点：会调整公开 stage-name API/JSON 字符串，调用方若依赖旧 CSV
需要直接迁移。

## Option B - Make Meter a real OutputRenderChain stage

在 renderer 中构建并 reset/process `LoudnessMeter`，让 `RenderedOutput` 返回 integrated
loudness、LRA、momentary/short-term 和 true peak 等分析结果。descriptor 与执行路径
由此一致。

优点：一次 render 同时获得权威结果，Meter 名称真实。缺点：所有 offline render 都
承担额外 ebur128/FIR true-peak CPU 和状态；需要定义 Meter 在 quantize 前还是后、
分析 compensated 还是 raw timeline，并扩展 public result/error/reset contracts。当前
renderer 的职责只是生成音频，这属于明显产品/API 扩展。

## Option C - Remove Meter descriptor only

从 canonical list 删除 Meter，quality bench 保持自行测量并手写说明。

优点：改动最小。缺点：没有结构化 post-render analysis 元数据，报告仍可能再次漂移；
没有解决“descriptor 是否为执行源”的总体问题。

## Recommendation

选择 Option A。该任务的目标是修复 metadata 真实性和重复编排，不应顺带让所有 offline
render 强制执行昂贵分析。把 Meter 作为显式 post-render analysis 后，未来可另加一个
opt-in `render_and_measure` API，而不污染基础 render contract。
