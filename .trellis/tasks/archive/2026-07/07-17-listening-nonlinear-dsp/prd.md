# 修复 Listening 与 Nonlinear DSP

## Goal

修复 saturation、noise shaper 与 Bauer-labelled crossfeed 的 P1 音质/模型错误，并把旧实现“能跑”的测试替换为连续性、量化边界、参考频响和实时性能均可复现的客观 oracle。目标是修复明确缺陷并提供证据，不宣称抽象意义上的“最佳音质”。

## Parent / Dependencies

* Parent：`../07-16-audio-core-quality-correctness/prd.md`
* Dependency：`07-17-audio-quality-performance-gates`（已完成）

## What I Already Know

* Saturation 在 threshold 处直接从 identity 跳到 driven waveshaper，且 full-band 只在 threshold 以上应用 output gain；两者都会产生有限跳变。
* Noise shaper 以 `1e-6` 固定幅度门控低电平并清空 history，TPDF 在正满幅附近还可能输出大于 1.0。
* Crossfeed 使用二阶 HPF；修复前 objective probe 为 80 Hz `-46.81 dB`、2 kHz `-9.18 dB`，频率方向与 Bauer 参考相反。
* 旧 quality gate 明确要求低频比高频更弱，因此必须连同算法一起更正，不能保留错误 oracle。
* canonical callback bench 已覆盖 Oversampled4x saturation、crossfeed 与 24-bit TPDF，适合作为同机修复前后性能证据。

## Requirements

### Saturation

* direct、Oversampled2x、Oversampled4x 与 high-pass exciter 共用固定最大 `0.05 FS` 的 C1 smoothstep soft-knee transfer；threshold 处值和一阶斜率连续。
* 保留现有 Tape/Tube/Transistor 基础 waveshaper、drive、mix 与 quality 枚举；本任务不顺带重设计全部音色或 oversampling 架构。
* saturation enabled 时，output gain 对最终 dry/wet 结果一致应用；disabled 仍精确 bypass，避免 threshold 两侧增益语义不同。

### Noise shaper

* enabled 且 channel 有效时，所有 finite 输入（包括 exact zero 与低于 -120 dBFS）均持续经过 TPDF/目标位深量化，不做固定幅度门控。
* 输出量化整数限制在 signed N-bit 范围，归一化输出范围为 `[-1.0, 1.0-LSB]`。
* NaN/±Inf 输出 `0.0` 并仅清空对应 channel history；不得让 non-finite 污染后续输出。
* adapter 只在 curve 真正变化时清空 curve history；enabled/bits 的独立更新不得无条件重置曲线状态。

### Bauer crossfeed

* 采用 libbs2b 参考的一阶 low-pass crossfeed + 本声道 high-boost + overload-prevention gain，默认 reference profile 为 700 Hz / 4.5 dB feed。
* 保留 `mix: 0..1`，但定义为 dry 到完整 reference output 的 strength：`0` 精确 bypass，`1` 完整 Bauer reference。
* mix 与 cutoff 更新采用约 10 ms 的逐样本 ramp，并保留滤波 history；真正的 sample-rate/新 stream 边界 snap 系数并 reset。
* stereo 以外布局继续透明 bypass，process 热路径保持无锁、无分配、无日志。

### Quality / performance evidence

* quality bench 增加 saturation threshold jump/slope、noise low-level/boundary、Bauer low-pass/reference/continuity 指标。
* 删除固化旧 HPF 行为的 crossfeed gates，换成低频交叉馈送强于高频及参考 steady-state oracle。
* 保留 saturation alias reduction 与 noise-band direction gates。
* 用同一环境、同一 quick callback scenarios 与修复前 JSON 做 median 对比；超过 10% 的同机回退必须修复或取得明确批准。
* Windows MSVC + MSYS2 SoXR 构建必须把 `libsoxr.dll` 及其同源 MinGW runtime closure 部署到 Cargo 的 binary/deps/examples 目录，使 test、example、bench 无需手工 PATH 注入即可直接运行。

## Acceptance Criteria

* [x] 三种 saturation type 在正负 threshold 邻域的最大幅度跳变与左右一阶斜率差通过 deterministic gate，非 0 dB output gain 不再重新引入跳变。
* [x] Direct 到 Oversampled4x 的现有 alias-reduction gate 继续通过；所有 quality mode 输出 finite/bounded 且稳态 process 无分配。
* [x] -140 dBFS、exact silence、正负满幅、越界 finite、NaN/±Inf 与 deterministic random stress 对所有 noise-shaper curve 满足量化网格、finite 和 signed target bound。
* [x] Bauer `mix=1` 的 steady-state direct/cross gain与独立参考公式一致；80 Hz crossfeed 明显强于 2 kHz，mono/multichannel 仍 bypass。
* [x] mix/cutoff runtime 更新不重置 state，ramp 结束后命中新目标；sample-rate/reset 能隔离旧 stream。
* [x] `audio_quality_measurements --quick --enforce`、callback quick report、fmt、Clippy、all/no-default feature tests 全部通过。
* [x] callback active 关键场景相对本任务修复前同机 median 无超过 10% 的未解释回退，且零分配断言继续通过。
* [x] Windows 上直接执行 quality/callback `cargo bench` 不再出现 `STATUS_DLL_NOT_FOUND`，并有构建期部署路径与 stale DLL 更新回归测试。

## Verification Evidence (2026-07-17)

* Quality quick：23/23 deterministic gates 通过，2 项缺失 EBU external corpus 继续明确标为 skipped；saturation jump `1.416e-6`、slope mismatch `3.610e-4`、4x alias reduction `16.32 dB`。
* Bauer：80 Hz / 2 kHz crossfeed 为 `-17.73 / -27.27 dB`，DC reference error `3.331e-16`，preserved-state mix delta `0`。
* Noise：-140 dBFS changed fraction `1.0`，signed stress peak `1.0`，non-finite outputs `0`。
* 修复前兼容 baseline 对比：active/no-convolver 512 median `116.885 ns/sample`（改善 `23.61%`），active/IR256 `124.369 ns/sample`（改善 `31.78%`）；全部 8 个 active DSP cases 通过 10% gate。bypass 的 sub-nanosecond measurement-floor 限制已在 research 诚实记录。
* Windows runtime：不注入 PATH 直接运行 quality 与 callback quick bench 均 exit 0；3 项 integration tests 覆盖 pkg-config prefix 解析、精确 runtime closure/三个 Cargo executable directories 和 stale DLL 刷新。
* Tests：all-features `275 unit + 8 benchmark support + 3 runtime deployment + 2 doctest`；no-default `267 + 8 + 3 + 2`，全部通过。
* Static/release：两套 `clippy --all-targets -D warnings`、rustfmt、`git diff --check`、`http`/`loudness-db` 单特性 check、`RUSTDOCFLAGS=-D warnings` rustdoc 全通过；`cargo package --all-features --allow-dirty` 打包 220 files / 1.7 MiB 并通过隔离编译。

## Definition of Done

* 缺陷机制先有在旧实现上失败的 unit/quality oracle，再修复根因。
* 文档、public comments、quality report 字段与实际频率/边界语义一致。
* 实现遵守实时线程无锁、无阻塞、无热路径分配约束。
* Trellis check、spec decision、工作提交、任务归档和 journal 全部完成。

## Technical Approach

* Saturation 使用 smoothstep soft knee 混合 identity 与既有 waveshaper，统一三条 processing path 的 transfer 和 output-gain 顺序。
* Noise shaper 删除 amplitude gate，在量化整数域 clamp，增加 channel-local invalid recovery，并修正 adapter 的无条件 curve reset。
* Crossfeed 按 libbs2b 系数公式实现固定 reference profile，再用平滑 strength 做 dry/reference blend；cutoff 用稳定的一阶系数插值。
* 在现有 custom quality/callback harness 中扩充指标，不新建重复 runner。

## Decision (ADR-lite)

**Context**：可以只做局部符号修复（HPF→LPF、删除 silence if），但那会遗漏 output gain 跳变、Bauer direct-path compensation、signed PCM 正端上限和参数切换瞬态。

**Decision**：采用 C1 soft-knee saturation、持续 dither + signed clamp、完整 Bauer reference topology + 10 ms 参数 ramp；允许产生可解释的输出变化，不追求旧实现位精确兼容。

**Consequences**：threshold 脉冲、低电平绕过和 crossfeed 频率方向得到结构性修复，oracle 可与解析/独立参考对齐；代价是 saturation knee、silence dither、crossfeed 电平与旧版本不同，必须更新文档、quality gates 和 callback 基线证据。

## Expansion Sweep / Future Evolution

* 预留未来新增 Cmoy/JMeier feed profile 的内部系数边界，但本次不增加 public preset/配置迁移。
* 若后续要做 precision-aware auto-dither，必须是显式策略并检测源 precision；不恢复固定 dBFS gate。
* Transistor 基础曲线和 oversampling filter 的独立重设计需另有 THD/alias/level-matching 任务，避免本次范围失控。

## Out of Scope

* 主观 ABX/听音室认证或“全球最佳”声明。
* 新 saturation type、全新 oversampler、自动 makeup gain。
* 新 crossfeed public profile enum、HRTF/room simulation、multichannel binaural renderer。
* Noise-shaper curve 系数扩容、precision-aware auto-off UI 或改变公开 bit-depth 范围。

## Research References

* [`research/saturation-continuity.md`](research/saturation-continuity.md) — C0/C1 方案比较与 soft-knee 选择。
* [`research/noise-shaper-boundaries.md`](research/noise-shaper-boundaries.md) — SoX 持续 dither、signed clipping 与 invalid recovery 契约。
* [`research/bauer-crossfeed.md`](research/bauer-crossfeed.md) — libbs2b low-pass/high-boost/gain 拓扑和参数连续性方案。
* [`research/quality-performance-gates.md`](research/quality-performance-gates.md) — 修复前基线、门禁替换与 CI 范围。

## Implementation Plan (small commits)

1. Saturation soft-knee、统一 output gain、连续性 unit/quality metrics。
2. Noise-shaper 低电平/边界策略、adapter state 修复及 stress metrics。
3. Bauer reference crossfeed、parameter ramp、adapter/oracle 更新。
4. 全矩阵检查、性能对比、spec/README 证据与 Trellis 收尾。
