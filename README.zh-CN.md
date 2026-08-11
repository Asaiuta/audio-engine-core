# audio-engine-core

[English](README.md) | **简体中文**

[![CI](https://github.com/Asaiuta/audio-engine-core/actions/workflows/ci.yml/badge.svg)](https://github.com/Asaiuta/audio-engine-core/actions/workflows/ci.yml)

> 一个面向实时安全的 Rust 音频处理核心,用于构建高品质音乐播放器。

`audio-engine-core` 提供解码、重采样、响度归一化、DSP 处理与流式流水线原语——而不接管音频设备、UI 或应用运行时。它从 Lyne 音频引擎中提取,作为与应用无关的核心层,将播放、设备输出与媒体库管理留给你的应用程序。

> ✅ 状态:1.0.0 — 稳定;公共 API 已文档化并受 SemVer 门禁保护,防止破坏性变更。要求 Rust 1.87+。

```text
┌──────────────────────────────────────────────────────────┐
│        你的应用  (UI · 播放 · 媒体库)                      │
└─────────────────────────────┬────────────────────────────┘
                              │
┌─────────────────────────────▼────────────────────────────┐
│                     audio-engine-core                    │
│   解码 → 重采样 → 响度 → DSP → 分析 → 流式               │
└─────────────────────────────┬────────────────────────────┘
                              │  (本 crate 不负责此层)
┌─────────────────────────────▼────────────────────────────┐
│              音频设备层  (CPAL / WASAPI / CoreAudio)      │
└──────────────────────────────────────────────────────────┘
```

## 为什么选择 audio-engine-core?

打造一款严肃的音乐播放器,会遇到与 UI 或播放列表关系不大的工程难题:

- **音频回调不能阻塞、不能分配** —— 错过一次截止时间就是一次可听见的爆音。
- **参数修改与音频处理竞争** —— 撕裂的、跨版本的参数读取。
- **源与设备采样率不同时的重采样** —— 要避免不可接受的伪影。
- **跨母带响度归一化** —— 相隔数十年制作的专辑能以接近的音量播放。
- **采样间峰值在处理后存续** —— 即使每个存储采样都低于满幅。
- **无缝与流式边界** —— 编解码器延迟/填充与 seek 行为必须恰好处理一次。

这些都以可复用、可度量、可测试的组件形式提供。

## 能力一览

| 领域 | 你能得到什么 |
| --- | --- |
| 解码 | 基于 Symphonia 0.6 的流式解码、面向不支持/损坏输入的类型化错误策略、按编解码器划分的无缝(gapsless)所有权 |
| 重采样 | SoX VHQ 流式重采样器(默认使用原生 SoXR 后端)或质量感知的纯 Rust 半带/FFT/sinc/多相路由,统一暴露在同一个 `process_checked` 接口之后 |
| 响度 | EBU R128 综合响度 + 真实峰值测量,离线分析加实时原子增益应用 |
| DSP | 10 段 IIR 双二阶 `Equalizer`、线性/最小相位 `FirEq`(经 `FFTConvolver` 应用)、Bauer 交叉馈送、带过采样抗混叠的饱和、面向长脉冲响应的分区路由 FFT 卷积、动态响度补偿、音量平滑、真实峰值限制器、噪声整形 |
| 实时控制 | 基于代际(generation)的无锁参数快照,用于将变更推入音频回调 |
| 流式 | 环形缓冲与流水线原语 |
| 分析 | 频谱分析器、AutoMix 分析、客观质量测量基准 |

## 快速开始

```toml
[dependencies]
audio-engine-core = "1"
```

测量一个文件的综合响度:

```rust
use std::path::Path;

use audio_engine_core::{LoudnessMeter, MediaLocation, StreamingDecoder};

fn analyze_file(path: &Path) -> Result<f64, Box<dyn std::error::Error>> {
    let location = MediaLocation::local(path.to_path_buf());
    let mut decoder = StreamingDecoder::open(location)?;
    let info = decoder.info();
    let mut meter = LoudnessMeter::new(info.channels, info.sample_rate)?;

    while let Some(samples) = decoder.decode_next()? {
        meter.process(&samples)?;
    }

    Ok(meter.integrated_loudness())
}
```

两个无需音频文件、无需可选特性的可运行示例:

- `resample_sine` —— 将合成的 48 kHz 正弦波经 SoX VHQ 重采样器流式处理为 44.1 kHz(精确游标推进,然后 `finish_checked`)。
- `equalizer_curve` —— 将立体声缓冲通过 10 段 `Equalizer` 处理。

```bash
cargo run --example resample_sine
cargo run --example equalizer_curve
```

## 实时安全设计

处理路径围绕一条不变量构建:音频回调内不允许分配、加锁、文件 I/O、日志或网络 I/O。预先分配并配置好处理器,然后通过原子快照类型更新参数:

```text
控制线程 (UI, 配置)                    音频回调 (每缓冲一次)
      │ set_* / 发布                          │ 快照读取
      ▼                                        ▼
┌──────────────────── 无锁原子快照 ───────────────────────┐
│  AtomicEqParams, AtomicVolumeParams, ... (无锁, 无分配)│
└─────────────────────────────────────────────────────────┘
```

每一个回调读取一整套缓存参数大约耗时 **7 ns**(基于代际的快照路径),而朴素的按字段拆分原子读取约为 50 ns,无条件的 `ArcSwap` 守卫加载约为 83 ns(`audio_lockfree_params_perf`;单机证据)。

处理器实现对象安全的 `StreamingProcessor` 生命周期。对于推荐的规范回调 DSP 顺序,构建由调用方驱动的 `PlaybackPipeline`。流水线既不拥有解码器也不拥有音频设备;回调保留其交错 `f64` 缓冲的所有权:

```rust
use audio_engine_core::{
    CallbackSpec, PlaybackConfig, PlaybackCrossfeedConfig, PlaybackLifecycleState,
    PlaybackPipeline,
};

let spec = CallbackSpec::stereo(48_000, 512)?;
let (mut pipeline, controller) = PlaybackPipeline::builder(spec)
    .configure(
        PlaybackConfig::transparent()
            .with_crossfeed(PlaybackCrossfeedConfig::enabled(0.25, 800.0)),
    )
    .build()?;

// controller 是独占的,因为它持有流水线私有的单消费者租约;
// 脉冲响应也通过它加载。其参数发布器可克隆,供 UI 与远程控制线程使用。
// 非有限值会被拒绝;有限但超出范围的值会被钳制。
let parameters = controller.parameters();
parameters.set_volume(0.8)?;
parameters.set_eq_band_gain_db(3, 2.5)?;

// 音频回调:在此处处理类型化结果,不要记录日志或 panic。
let mut samples = [0.0_f64; 512 * 2];
let progress = pipeline.process(&mut samples)?;

// 在控制线程切换曲目,而流水线存活于回调之中:
// 淡出、在块边界排空尾部,然后重新武装。
controller.request_stop_with_fade(20)?;
while pipeline.lifecycle_state() != PlaybackLifecycleState::Idle {
    let _ = pipeline.process(&mut samples)?;
}
controller.request_reset();
# Ok::<(), audio_engine_core::ProcessError>(())
```

`CallbackSpec` 描述的是已转换到设备域的音频,并限制回调块的最大尺寸。`PlaybackConfig::transparent()` 是默认值:它禁用所有非恒等阶段(包括限制器),因此逐样本保留输入,且不引入限制器延迟。对于已预备容量的块,`PlaybackPipeline::process` 不进行任何分配。

生命周期转换从控制线程请求、由回调在块边界应用,因为流水线被移入回调后,无法从别处可变借用:`request_reset`、`request_drain` 与 `request_stop_with_fade` 均无锁、无分配,`PlaybackController::lifecycle_status` 报告已应用的请求代际。排空期间,回调块被剩余效果尾部覆盖(上限由 `PlaybackConfig::with_drain_policy` 决定);进入终止态后,`process` 写入静音并持续成功返回,因为设备回调不会因曲目结束而停止触发。`finish_into_with_policy` 与 `reset` 仍是面向在回调之外持有流水线的集成方的直接控制线程操作。

数值契约:构建期配置被严格校验——非有限或超出范围的 `PlaybackConfig` 值会使 `build()` 以 `ProcessError::InvalidParameter` 失败。运行期参数写入以同样的类型化错误拒绝非有限值,并将有限值钳制到文档化范围(`VOLUME_MIN`/`VOLUME_MAX`、`EQ_BAND_GAIN_DB_MIN`/`_MAX` 及其他导出的范围常量),每个 `PlaybackParameters` 读取器返回的都是在实际生效的值。回调音量只做衰减(0.0–1.0);正向增益请在上游施加。

`PlaybackController` 刻意设计为独占的,因为它保留私有卷积器租约。卷积通过它加载:`controller.load_impulse_response(&ir)?` 会对照回调规格校验交错 IR,并在控制线程上预备 FFT 核,音频回调采用它时不作任何分配。饱和(saturation)武装是构建期决策,因为它固定了该阶段的延迟;但已武装的阶段接受运行期的 drive/threshold/mix/type/gain 变更与软旁路。`controller.parameters()` 返回的 `PlaybackParameters` 是安全的可克隆 UI/远程更新句柄。动态响度遥测是尽力而为的按字段最新值报告,而非多值一致快照。如需自定义阶段顺序或底层原子控制,可直接使用低层 `OutputChainBuilder` / `StreamingProcessor` API。

```rust
use audio_engine_core::processor::traits::{
    process_checked, AudioBlockMut, ProcessBuffers, ProcessError, ProcessProgress,
    StreamingProcessor,
};

fn process_callback_block(
    processor: &mut dyn StreamingProcessor,
    samples: &mut [f64],
    channels: usize,
) -> Result<ProcessProgress, ProcessError> {
    let block = AudioBlockMut::new(samples, channels)?;
    process_checked(processor, ProcessBuffers::in_place(block))
}
```

### 迁移说明

原有的 `AudioProcessor` / `ProcessResult` API 已移除;适配器直接实现 `StreamingProcessor`。`DspChain::process` / `reset` / `set_sample_rate` 返回类型化结果,回调集成方必须在音频线程上处理失败而不记录日志或 panic。固定(fixed)处理器保留零拷贝原地快路径并实现 `FixedInPlaceProcessor`——这是 `DspChain::add` 要求的准入契约。`DspChain::new` 与 `with_capacity` 返回 `Result` 并拒绝零采样率;链没有随意的 `Default`。启用/静音控制归属于具体原子参数句柄和 `ConvolverControl`,而非基础流式生命周期。就地外(out-of-place)调用使用调用方提供的输出缓冲,并显式报告 `NeedInput` / `NeedOutput` 背压。
`StreamingResampler` 遵循同样的就地外契约:从 `ProcessProgress` 推进两个游标,经 `finish_checked` 用原生 SoXR `drain()` 结束流,用 `reset()` 清除原生历史(旧的 `process_chunk_*` / `flush_*` 辅助函数无法表达部分消费的输入)。离线 `OutputRenderChain::render` 默认采用补偿时间线——累计算法延迟在最终输出采样率上被一次性移除,而有限的语义效果尾部被保留;`OfflineRenderPolicy::raw_causal()` 保留前导延迟与全部终结输出。未知/无限尾部使用可配置的预抖动 RMS 阈值、持续静音驻留与硬上限,达到上限时设置 `RenderedOutput::tail_truncated`。`OutputChainParams` 只携带回调/输出域配置;构建离线渲染器时请传入输入采样率 `build_render_chain(source_rate)` 或 `build_render_chain_with_policy(source_rate, policy)`。

## 质量与验证

本项目把音频质量当作需要测量的东西,而不只是聆听。`benches/` 中的基准针对公共 API 运行,并分析渲染后的 `f64` 缓冲:

| 领域 | 测量内容 |
| --- | --- |
| 响度 | 与参考实现对比的 EBU R128 一致性 |
| 真实峰值 | 过采样采样间峰值检测 |
| 重采样 | 通带偏差、混叠衰减、THD+N |
| EQ | 目标响应精度 |
| 饱和 | 折叠混叠能量 |
| 卷积 | 与 overlap-save 参考对比的 IR 正确性 |
| 实时控制 | 参数变更连续性 |

来自单机、单一配置的代表性结果(用 `cargo bench` 复现;数值随 CPU、编译器与负载而异):

- `LoudnessMeter` 综合响度与直接 `ebur128` 对比: **0.000000 LU**
- 重采样 THD+N,44.1 kHz → 48 kHz:**-187.0 dB**(默认 SoXR 后端;纯 Rust rubato UltraHigh 测得 -204.9 dB,见 [docs/quality.md](docs/quality.md))
- 最差拟合混叠衰减,96 kHz → 48 kHz:**-290.2 dB**(接近分析器自身的数值下限)
- 真实峰值限制器:在 +0.10 dBTP 采样间应力信号上达到 **-1.00 dBTP**(旧式采样峰值模式从不触发:+0.10 dBTP)
- 动态响度低音量补偿:**40 Hz 处 +8.41 dB / 3 kHz 处 +2.83 dB**

在离线渲染链中,限制器运行于输出采样率域,位于任何重采样之后,因此只有最终量化在其下游——且限制器的上限已为量化器的有界误差预留余量(派生的输出上限守卫)。完整输出链真实峰值探针作为 CI 门禁强制执行:当前快速运行测得最坏全链输出真实峰值为 -1.000 dBTP,超限点为零。

在 2026-07-27 固定核心的重型适配器控制上,SoXR v2 测得 44.1→48 / 48→44.1 kHz 为 8.569 / 7.424 ns/输入采样,而原生 libsoxr 为 8.632 / 7.368,双向统计持平。在独立的同几何 Rubato 构建中,v17 测得 8.182 / 7.025,原生 Rubato 为 8.592 / 6.908:正向快 4.77%,反向持平。更广的 11 引擎矩阵是不同方案、通道与延迟策略下的帕累托证据,而非普适的"最快"排名。

完整的 crate 内基准命令、JSON 报告/基线机制、处理预算表与完整测量表位于 [docs/quality.md](docs/quality.md)。上游原始与独立重采样方法学及结果位于 [docs/resampler-comparison.md](docs/resampler-comparison.md)。

## 安装与功能开关

以下四个 Cargo 特性互相独立;前三个默认启用:

- `http`(默认):经 `reqwest` 的 HTTP/HTTPS 流式解码,包括 Range 流式与整文件下载回退。`MediaLocation` 独立于此特性校验本地与 HTTP 身份。关闭 `http` 时,HTTP 位置返回 `DecoderError::FeatureUnavailable`;`reqwest` 与 `NetworkError` 类型不参与编译。
- `loudness-db`(默认):基于 SQLite 的响度元数据持久化(`LoudnessDatabase`、`TrackLoudness`、`LoudnessSourceIdentity`、`DatabaseStats`,经 `rusqlite`)。缓存键使用带命名空间的 SHA-256 身份;签名 HTTP URL 从不以明文存储,无验证器的 HTTP 记录始终视为过期。关闭此特性后,EBU R128 辅助类型(`LoudnessMeter`、`LoudnessNormalizer`、`TruePeakDetector`)仍可用;仅移除磁盘缓存。
- `soxr`(默认):原生 SoXR 重采样后端(SoX VHQ)。构建/链接时需要 libsoxr 原生库;libsoxr 为 LGPL-2.1(见 [许可证](#许可证))。
- `rubato`:质量感知的纯 Rust 后端。`PhaseResponse::Linear` + High 下的精确 2 倍上采样使用专用 127 抽头对称半带 FIR;其他常见比率使用 FFT,High 采用两个子块、UltraHigh 采用一个更长子块,仅病态降比率使用窗函数 sinc。`Minimum` 与 `Maximum` 使用设计阶段确定的有理 FIR 与实倒谱谱分解,小插值因子选择频谱执行,其余选择连续多相执行;超过 1024 的降比率分量被拒绝而非静默按线性相位处理。无原生依赖。至少必须启用一个重采样后端——两者都不启用是编译错误;两者都启用时 `soxr` 优先。

无原生依赖的纯 Rust、纯 DSP 构建:

```toml
audio-engine-core = { version = "1", default-features = false, features = ["rubato"] }
```

默认 SoXR 后端的 Windows(vcpkg 或 MSYS2)与 Unix 安装说明见 [docs/installation.md](docs/installation.md)。

## 范围(Scope)

本 crate 拥有音频处理层。它刻意不拥有设备管理(CPAL/WASAPI 输出流)、桌面 UI 或 Tauri 集成、播放队列逻辑、媒体库扫描、HTTP/WebSocket 服务路由、WebDAV 或网易云集成,以及应用运行时目录——这些都留在 Lyne 应用 crate 中,且不为每个 Lyne 内部用例提供稳定兼容层。这种分离使核心能够嵌入不同的应用与输出后端之下。

## 适合谁

如果你符合以下情况,则很适合:

- 正在用 Rust 构建音乐播放器,需要一个处理核心垫底;
- 正在组装自定义实时音频流水线;
- 正在试验高品质 DSP(EQ、交叉馈送、饱和、卷积);
- 正在编写离线响度分析工具。

以下情况可能不适合:你需要完整播放器、高层播放 API,或音频设备抽象。

## 解码与格式支持

解码基于 [Symphonia](https://github.com/pdeljanov/Symphonia) 0.6,其内置编解码器/容器全部编译启用(如 WAV、FLAC、MP3、AAC/MP4、OGG/Vorbis);本 crate 不添加自定义编解码器,并使支持边界显式且可测试。`StreamingDecoder` 通过只读的 `decoder.info()` 暴露解码后的采样率、声道数,以及(已知时)总帧数与时长,包括尽力而为的位置信息 `decoder.info().channel_layout`。它只用于观察:解码器依赖同样的值进行分段、无缝修剪与 seek 运算,因此不是一个调用方可写的控制通道。

- **不支持/无法识别的输入**返回类型化 `DecoderError::UnsupportedFormat`;容器探测成功但没有可解码音轨时返回 `DecoderError::NoAudioTrack`。
- **损坏或截断的输入**有明确策略:解码器要么返回类型化错误,要么产出能恢复的部分样本——它绝不 panic,也绝不静默地报告对缺失数据的完整解码。
- **无缝(gapsless)所有权**是按编解码器显式划分的:Symphonia 拥有 MP3 与 Vorbis 的分组修剪/重置行为;其他编解码器保留本 crate 的 Track 级延迟/填充回退。两条路径互斥,因此延迟或填充不会被修剪两次。回退只能修剪容器声明的内容:Symphonia 0.6 的 MP4 demuxer 不暴露 AAC priming/padding 元数据(如 `iTunSMPB`),因此 M4A/AAC 当前播放时不修剪,而 CAF 两者都声明并被精确修剪。
- **Seek** 仅使用 Symphonia 的 `SeekMode::Coarse`;刻意不暴露样本精确(`Accurate`)模式。粗粒度 seek 落在请求时间之前或等于请求时间的分组/帧边界——有界不精确性记录为 `StreamingDecoder::SEEK_COARSE_TOLERANCE_FRAMES`,实际落点可通过 `decoder.current_frame()` 读取。Track 级编码器延迟只在流的真正起点生效;原生 MP3/Vorbis 解码器在 seek 后消费其分组局部的修剪与重置预滚。

## 项目状态

稳定 `1.0.0` 发布:公共 API 完整文档化(编译期拒绝 `missing_docs`),由提交的公共面快照冻结,并由 CI 中的 SemVer 门禁守护——任何破坏性变更都会在发布前使构建失败。要求 Rust 1.87+(Symphonia 0.6 本身要求 1.85;更高的 crate MSRV 反映本仓库中现有的 DSP 代码)。作为 Lyne 播放器的音频基础用于生产环境。破坏性变更按 [CONTRIBUTING.md](CONTRIBUTING.md) 中的策略保留给主版本号升级;重要变更记录在 [CHANGELOG.md](CHANGELOG.md)。

## 许可证

任选其一:

- Apache License, Version 2.0([LICENSE-APACHE](LICENSE-APACHE) 或 <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license([LICENSE-MIT](LICENSE-MIT) 或 <http://opensource.org/licenses/MIT>)

除非你明确另行声明,否则按 Apache-2.0 许可的定义,你有意为作品提交的任何贡献,均按上述双许可授权,不附加任何额外条款或条件。

### 原生依赖许可

启用默认 `soxr` 特性时,本 crate 链接 SoXR 原生库(libsoxr),其以 LGPL-2.1 分发。本 crate 的 Rust 源码为 MIT OR Apache-2.0,但静态链接 libsoxr 的二进制带有 LGPL-2.1 重链接义务。使用 `default-features = false` 与纯 Rust `rubato` 后端构建时不链接 libsoxr,不承担 LGPL 义务。详见 [NOTICE](NOTICE)。