# audio-engine-core

[English](README.md) | **简体中文**

[![CI](https://github.com/Asaiuta/audio-engine-core/actions/workflows/ci.yml/badge.svg)](https://github.com/Asaiuta/audio-engine-core/actions/workflows/ci.yml)

> **一个面向 Rust 的实时安全、可度量的音频处理核心。**

`audio-engine-core` 是一个与应用无关的 Rust 音频核心,用于构建高品质音乐播放器与实时音频应用。

它提供解码后音频与输出设备之间的处理基础设施:

**解码 → 重采样 → 响度 → DSP → 分析 → 流式**

本核心刻意**不**拥有你的 UI、音频设备、播放运行时或媒体库。它被设计为位于应用之下、专职的音频处理层。

> **生产来源:** `audio-engine-core` 从 Lyne 音频引擎中提取,作为其与应用无关的核心层。

> **状态:** `1.0.0` — 公共 API 稳定,已文档化并受 SemVer 门禁保护,防止破坏性变更。要求 Rust 1.87+。

---

## 为什么选择 audio-engine-core?

一款严肃的音乐播放器不只是 DSP 算法的集合。

困难之处在于:让这些算法在实时音频回调的约束下协同工作,同时保持音频质量、确定性行为与可预测的所有权。

`audio-engine-core` 围绕三个一等设计目标构建:

|                     |                                                                                                                   |
| ------------------- | ----------------------------------------------------------------------------------------------------------------- |
| **实时安全**         | 音频回调内无分配、无阻塞锁、无 I/O、无日志、无失控失败                                                        |
| **音频质量**         | 高质量重采样、响度归一化、真实峰值处理、抗混叠 DSP、无缝感知流式                                              |
| **可度量性**         | 公共 API 基准、参考对比、客观质量指标、CI 强制不变量                                                             |

目标不是提供最大的音频效果集合。

目标是提供一个**可在实时播放系统内部被信任的可组合音频处理核心。**

---

## 有何不同?

### 结构性实时安全

音频回调被当作硬实时边界对待。

处理器在进入回调之前预备完成。运行时控制通过无锁原子参数快照传递,而非与 UI 或控制线程直接同步。

```text
控制 / UI 线程                          音频回调
       │                                      │
       │ 发布参数                              │ 快照
       ▼                                      ▼
┌─────────────────────────────────────────────────────────┐
│        基于代际的原子参数快照                            │
│        无锁 · 无分配 · 版本一致                         │
└─────────────────────────────────────────────────────────┘
```

回调路径被设计为避免:

* 堆分配
* 互斥锁与阻塞同步
* 文件或网络 I/O
* 日志记录
* 基于 panic 的控制流

基于代际的参数快照路径(基准名 `audio_lockfree_params_perf`)在基准机器上测得约 **7 ns/回调**,而朴素的拆分原子读取约为 **50 ns**,无条件的 `ArcSwap` 守卫加载约为 **83 ns**。

这些是单机基准结果,不是普遍硬件保证。

---

### 不止是 DSP 集合

本项目将解码、重采样、响度处理、DSP、分析与流式原语组合进单一处理架构。

```text
                         audio-engine-core

┌──────────────┐
│    解码      │
│  Symphonia   │
└──────┬───────┘
       │
       ▼
┌──────────────┐
│   重采样     │
│ SoX VHQ /    │
│ 纯 Rust      │
└──────┬───────┘
       │
       ▼
┌──────────────┐
│    响度      │
│ EBU R128 /   │
│  真实峰值    │
└──────┬───────┘
       │
       ▼
┌──────────────────────────────────────────────┐
│                     DSP                      │
│                                              │
│ EQ · FIR EQ · 交叉馈送 · 卷积                │
│ 饱和 · 动态响度 · 限制器                     │
│ 音量 · 噪声整形                              │
└──────────────────────┬───────────────────────┘
                       │
                       ▼
┌──────────────┐
│    分析      │
│ 频谱 /       │
│ AutoMix /    │
│ 测量         │
└──────┬───────┘
       │
       ▼
┌──────────────┐
│    流式      │
│ 环形缓冲     │
│ 流水线       │
└──────────────┘
```

播放状态、UI、媒体库管理、网络与设备输出仍由应用负责。

---

### 音频质量是测量出来的,不是声称出来的

音频质量被当作一种应可度量的工程属性。

本项目包含针对质量敏感组件的客观验证,包括:

* EBU R128 响度一致性
* 真实峰值 / 采样间峰值检测
* 重采样器通带行为
* 重采样器混叠衰减
* 重采样器 THD+N
* EQ 响应精度
* 饱和混叠能量
* 卷积正确性
* 实时参数连续性
* 全链输出真实峰值上限

基准针对公共 API 运行,分析渲染后的音频缓冲,而非私有实现捷径。

完整方法学与可复现命令记录在 `docs/quality.md`。

---

## 架构

`audio-engine-core` 刻意止步于应用层与设备层之前。

```text
┌──────────────────────────────────────────────────────────┐
│                      你的应用                             │
│                                                          │
│       UI · 播放 · 媒体库 · 网络 · 运行时                 │
└──────────────────────────┬───────────────────────────────┘
                           │
                           ▼
┌──────────────────────────────────────────────────────────┐
│                  audio-engine-core                       │
│                                                          │
│ 解码 → 重采样 → 响度 → DSP → 分析 → 流式                │
└──────────────────────────┬───────────────────────────────┘
                           │
                           ▼
┌──────────────────────────────────────────────────────────┐
│                      设备层                              │
│             CPAL · WASAPI · CoreAudio · ...             │
└──────────────────────────────────────────────────────────┘
```

这种分离是有意为之。

核心拥有**音频处理**,而非应用。

这使得同一处理层可以跨不同播放器、运行时与输出后端复用,而无需继承整个应用架构。

---

## 核心能力

| 领域                  | 能力                                                                                                 |
| --------------------- | ---------------------------------------------------------------------------------------------------------- |
| **解码**            | 基于 Symphonia 0.6 的流式解码、类型化错误处理、按编解码器划分的无缝(gapless)所有权                          |
| **重采样**        | SoX VHQ / SoXR 后端与质量感知的纯 Rust 重采样后端,统一在一个流式接口之后                                     |
| **响度**          | EBU R128 综合响度、真实峰值测量、离线分析与实时归一化                                                        |
| **EQ**                | 10 段 IIR 双二阶 EQ 与线性/最小相位 FIR EQ                                                                     |
| **卷积**       | 面向长脉冲响应的分区路由 FFT 卷积                                                                        |
| **交叉馈送**         | Bauer 交叉馈送                                                                                            |
| **饱和**        | 带抗混叠的过采样饱和                                                                                   |
| **动态响度**  | 基于感知等响曲线的响度补偿                                                                          |
| **限制器**           | 真实峰值限制                                                                                         |
| **音量**            | 平滑的实时音量控制                                                                           |
| **噪声**             | 噪声整形 / 抖动(dithering)支持                                                                          |
| **实时控制**  | 基于代际的无锁原子参数快照                                                                      |
| **流式**         | 环形缓冲与流水线原语                                                                        |
| **分析**          | 频谱分析、AutoMix 分析与客观测量基准                                                                     |
| **离线渲染** | 延迟补偿渲染与可配置的效果尾部策略                                                                        |

---

## 实时处理模型

推荐的高层 API 是 `PlaybackPipeline`。

```rust
use audio_engine_core::{
    CallbackSpec,
    PlaybackConfig,
    PlaybackCrossfeedConfig,
    PlaybackLifecycleState,
    PlaybackPipeline,
};

let spec = CallbackSpec::stereo(48_000, 512)?;

let (mut pipeline, controller) = PlaybackPipeline::builder(spec)
    .configure(
        PlaybackConfig::transparent()
            .with_crossfeed(
                PlaybackCrossfeedConfig::enabled(0.25, 800.0)
            ),
    )
    .build()?;

let parameters = controller.parameters();

parameters.set_volume(0.8)?;
parameters.set_eq_band_gain_db(3, 2.5)?;

let mut samples = [0.0_f64; 512 * 2];

// 音频回调。
let progress = pipeline.process(&mut samples)?;
```

`CallbackSpec` 描述已转换到设备域的音频,并限制回调块的最大尺寸。

`PlaybackConfig::transparent()` 提供恒等导向的默认值:非恒等阶段(如限制器)被禁用,逐样本保留输入,且不引入限制器延迟。

对于已预备容量的块,`PlaybackPipeline::process` 不进行任何分配。

### 控制与生命周期

播放生命周期转换从控制侧请求、由音频回调在块边界应用。

```text
控制线程
     │
     │ request_stop_with_fade()
     ▼
┌─────────────────────────┐
│  原子生命周期请求        │
└────────────┬────────────┘
             │
             ▼
      音频回调
             │
             ├── 淡出
             ├── 排空效果尾部
             ├── 进入终止状态
             └── 输出静音
```

这让流水线在保持由回调拥有的同时,仍支持:

* 无锁生命周期请求
* 块边界状态转换
* 淡出
* 效果尾部排空
* 重置与重新武装
* 无缝播放集成

曲目进入终止状态后回调仍持续成功返回,因为音频设备回调不会仅仅因为曲目结束而停止触发。

由于流水线被移入回调,无法从别处可变借用:`request_reset`、`request_drain` 与 `request_stop_with_fade` 均无锁、无分配,`PlaybackController::lifecycle_status` 报告已应用的请求代际。排空期间,回调块被剩余效果尾部覆盖(上限由 `PlaybackConfig::with_drain_policy` 决定);进入终止态后,`process` 写入静音并持续成功。对于在回调之外持有流水线的集成方,`finish_into_with_policy` 与 `reset` 仍是直接的控制线程操作。

### 参数安全

运行时参数变更围绕显式契约设计。

构建期配置被严格校验:

* 非有限值被拒绝
* 无效范围返回类型化 `ProcessError`
* 运行时参数写入拒绝非有限值
* 有限但超出范围的值被钳制到文档化上限
* 参数读取器返回实际生效的值

例如导出的音量与 EQ 增益范围常量(`VOLUME_MIN` / `VOLUME_MAX`、`EQ_BAND_GAIN_DB_MIN` / `_MAX` 及其他导出的范围常量)。回调音量只做衰减(0.0–1.0);正向增益请在上游施加。

参数发布器可克隆,供 UI 或远程控制线程使用,无需这些线程直接触碰实时处理状态。

`PlaybackController` 刻意设计为独占的,因为它保留私有卷积器租约。卷积通过它加载:`controller.load_impulse_response(&ir)?` 会对照回调规格校验交错 IR,并在控制线程上预备 FFT 核,音频回调采用它时不作任何分配。饱和(saturation)武装是构建期决策,因为它固定了该阶段的延迟;但已武装的阶段接受运行期的 drive/threshold/mix/type/gain 变更与软旁路。`controller.parameters()` 返回的 `PlaybackParameters` 是安全的可克隆 UI/远程更新句柄。动态响度遥测是尽力而为的按字段最新值报告,而非多值一致快照。

如需自定义处理图或底层原子控制,请使用 `OutputChainBuilder` 与 `StreamingProcessor` API。

---

## 流式处理器模型

处理器实现对象安全的 `StreamingProcessor` 生命周期。

```rust
use audio_engine_core::processor::traits::{
    process_checked,
    AudioBlockMut,
    ProcessBuffers,
    ProcessError,
    ProcessProgress,
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

流式 API 通过 `ProcessProgress` 显式表示输入/输出的部分消费。

这避免了关于块尺寸的隐藏假设,并使背压对调用方可见。

---

## 重采样

重采样通过统一的流式契约暴露。

本项目支持:

### SoX VHQ / SoXR

默认原生后端通过 libsoxr(SoX VHQ 质量)提供高质量 SoX 重采样。构建/链接时需要 libsoxr 原生库(LGPL-2.1;见 [许可证](#许可证))。

### 纯 Rust

可选的 `rubato` 后端提供质量感知的纯 Rust 路由,包括:

* 精确 2× 转换的半带 FIR 路径(`PhaseResponse::Linear` + High 使用专用 127 抽头对称半带 FIR)
* 常见比率的 FFT 路径(High 采用两个子块,UltraHigh 采用一个更长子块)
* 病态降比率的窗函数 sinc 处理
* Minimum/Maximum 相位使用设计阶段确定的有理 FIR 与实倒谱谱分解,小插值因子选择频谱执行,其余选择连续多相执行
* 超过 1024 的降比率分量被拒绝,而非静默按线性相位处理
* 无原生依赖

流式契约显式跟踪已消费与已产出的样本。

收尾使用 `finish_checked()` 排空原生重采样器状态,`reset()` 清除流式历史。

至少必须启用一个重采样后端 —— 两者都不启用是编译错误;两者都启用时 `soxr` 优先。

详细对比与方法学见 `docs/resampler-comparison.md`。

---

## 响度归一化

响度子系统将离线分析与实时应用分离。

```text
离线分析
      │
      ├── 综合响度
      ├── 真实峰值
      └── 曲目元数据
              │
              ▼
       持久化元数据
              │
              ▼
实时播放
      │
      └── 原子增益应用
```

这让昂贵的分析发生在实时回调之外,而回调只执行应用结果所需的最小运行时工作。

实现以 EBU R128 响度测量为目标,并包含真实峰值分析。离线分析、归一化辅助类型(`LoudnessMeter`、`LoudnessNormalizer`、`TruePeakDetector`)与实时原子增益应用在默认特性下全部可用;可选的曲目响度 SQLite 持久化见 [功能开关](#功能开关)。

---

## 离线渲染

同一处理架构也可用于离线渲染。

`OutputRenderChain::render` 支持延迟感知渲染策略与效果尾部处理。

默认补偿时间线在最终输出采样率上一次性移除累计算法延迟,同时保留有限的语义效果尾部。

`OfflineRenderPolicy::raw_causal()` 也可以改为保留前导因果延迟。

对于未知或实际上无限的尾部,渲染可以使用:

* 可配置的 RMS 阈值
* 持续静音驻留
* 硬上限

达到硬上限时,`RenderedOutput::tail_truncated` 记录截断。

这使离线渲染保持确定性,而无需每个处理器都暴露无限或无界尾部。

在离线渲染链中,限制器运行于输出采样率域,位于任何重采样之后,因此只有最终量化在其下游 —— 且限制器的上限已为量化器的有界误差预留余量(派生的输出上限守卫)。

---

## 质量与验证

当前基准套件的代表性结果(用 `cargo bench` 复现;数值随 CPU、编译器与负载而异)包括:

| 测量项                                                    |                 结果 |
| -------------------------------------------------------------- | ----------------------: |
| `LoudnessMeter` 综合响度与直接 `ebur128` 对比 |         **0.000000 LU** |
| SoXR 重采样器 THD+N,44.1 → 48 kHz                            |           **−187.0 dB** |
| 纯 Rust Rubato UltraHigh THD+N                               |           **−204.9 dB** |
| 最差拟合混叠衰减,96 → 48 kHz                    |           **−290.2 dB** |
| 真实峰值限制器上限                                      |          **−1.00 dBTP** |
| 动态响度补偿,40 Hz / 3 kHz                 | **+8.41 dB / +2.83 dB** |
| 基于代际的参数快照(`audio_lockfree_params_perf`) |            **~7 ns** |

真实峰值限制器上下文:**−1.00 dBTP** 是在 +0.10 dBTP 采样间应力信号上测得的;旧式采样峰值模式对该信号从不触发(+0.10 dBTP)。完整输出链真实峰值探针作为 CI 门禁强制执行:当前快速运行测得最坏全链输出真实峰值为 **−1.000 dBTP**,超限点为零。

这些数值是特定机器、配置与编译器构建下的基准证据。

它们**不是普遍的性能或质量保证**。

### 重采样器性能

在 2026-07-27 固定核心的重型适配器控制上:

```text
SoXR v2
44.1 → 48 kHz   : 8.569 ns/采样    (原生 libsoxr: 8.632)
48 → 44.1 kHz   : 7.424 ns/采样    (原生 libsoxr: 7.368)
  → 双向统计持平

Rubato v17(同几何构建)
44.1 → 48 kHz   : 8.182 ns/采样    (原生 Rubato: 8.592)
48 → 44.1 kHz   : 7.025 ns/采样    (原生 Rubato: 6.908)
  → 正向快 4.77%,反向持平
```

本项目刻意将其作为基准证据报告,而非声称普遍的"最快"后端。更广的 11 引擎矩阵是不同方案、通道与延迟策略下的帕累托证据,而非普适的最快排名。

完整方法学、配置、原始测量与可复现命令见:

* `docs/quality.md`
* `docs/resampler-comparison.md`

---

## 安装

```toml
[dependencies]
audio-engine-core = "1"
```

最小的纯 Rust、纯 DSP 构建(无原生依赖):

```toml
[dependencies]
audio-engine-core = {
    version = "1",
    default-features = false,
    features = ["rubato"]
}
```

至少必须启用一个重采样后端。

---

## 功能开关

主要的 Cargo 特性:

| 特性       | 默认 | 用途                                     |
| ------------- | :-----: | ------------------------------------------- |
| `http`        |    ✓    | HTTP/HTTPS 流式解码                 |
| `loudness-db` |    ✓    | 基于 SQLite 的响度元数据持久化 |
| `soxr`        |    ✓    | 原生 SoXR / SoX VHQ 重采样            |
| `rubato`      |         | 纯 Rust 质量感知重采样          |

四个特性互相独立;前三个默认启用。

### `http`

经 `reqwest` 提供 HTTP/HTTPS 流式解码,包括 Range 流式与整文件下载回退。

`MediaLocation` 独立于此特性校验本地与 HTTP 身份。关闭此特性时,HTTP 位置返回 `DecoderError::FeatureUnavailable`;`reqwest` 与 `NetworkError` 类型不参与编译。

### `loudness-db`

经 `rusqlite` 提供响度元数据的 SQLite 持久化(`LoudnessDatabase`、`TrackLoudness`、`LoudnessSourceIdentity`、`DatabaseStats`)。

缓存身份使用带命名空间的 SHA-256 身份。签名 HTTP URL 不以明文存储,无验证器的 HTTP 记录始终视为过期。

关闭此特性后,EBU R128 测量、归一化与真实峰值 API 仍可用;仅移除磁盘持久化缓存。

### `soxr`

启用原生 SoXR 后端。

构建/链接时需要 libsoxr 原生库。libsoxr 为 LGPL-2.1;见 [许可证](#许可证)。Windows(vcpkg 或 MSYS2)与 Unix 安装说明见 `docs/installation.md`。

### `rubato`

启用纯 Rust 重采样后端。

当 `soxr` 与 `rubato` 同时启用时,SoXR 被选为默认后端。

---

## 快速开始

测量综合响度:

```rust
use std::path::Path;

use audio_engine_core::{
    LoudnessMeter,
    MediaLocation,
    StreamingDecoder,
};

fn analyze_file(
    path: &Path,
) -> Result<f64, Box<dyn std::error::Error>> {
    let location = MediaLocation::local(path.to_path_buf());
    let mut decoder = StreamingDecoder::open(location)?;

    let info = decoder.info();

    let mut meter =
        LoudnessMeter::new(info.channels, info.sample_rate)?;

    while let Some(samples) = decoder.decode_next()? {
        meter.process(&samples)?;
    }

    Ok(meter.integrated_loudness())
}
```

可运行示例:

```bash
cargo run --example resample_sine
cargo run --example equalizer_curve
```

示例无需外部音频文件或可选特性。

---

## 解码与格式支持

解码基于 [Symphonia](https://github.com/pdeljanov/Symphonia) 0.6,其内置编解码器/容器全部编译启用(如 WAV、FLAC、MP3、AAC/MP4、OGG/Vorbis);本 crate 不添加自定义编解码器,并使支持边界显式且可测试。

`StreamingDecoder` 通过只读的 `decoder.info()` 暴露解码后的采样率、声道数,以及(已知时)总帧数与时长,包括尽力而为的位置信息 `decoder.info().channel_layout`。它只用于观察:解码器依赖同样的值进行分段、无缝修剪与 seek 运算,因此不是一个调用方可写的控制通道。

* **不支持/无法识别的输入**返回类型化 `DecoderError::UnsupportedFormat`;容器探测成功但没有可解码音轨时返回 `DecoderError::NoAudioTrack`。
* **损坏或截断的输入**有明确策略:解码器要么返回类型化错误,要么产出能恢复的部分样本 —— 它绝不 panic,也绝不静默地报告对缺失数据的完整解码。
* **无缝(gapless)所有权**是按编解码器显式划分的:Symphonia 拥有 MP3 与 Vorbis 的分组修剪/重置行为;其他编解码器保留本 crate 的 Track 级延迟/填充回退。两条路径互斥,因此延迟或填充不会被修剪两次。回退只能修剪容器声明的内容:Symphonia 0.6 的 MP4 demuxer 不暴露 AAC priming/padding 元数据(如 `iTunSMPB`),因此 M4A/AAC 当前播放时不修剪,而 CAF 两者都声明并被精确修剪。
* **Seek** 仅使用 Symphonia 的 `SeekMode::Coarse`;刻意不暴露样本精确(`Accurate`)模式。粗粒度 seek 落在请求时间之前或等于请求时间的分组/帧边界 —— 有界不精确性记录为 `StreamingDecoder::SEEK_COARSE_TOLERANCE_FRAMES`,实际落点可通过 `decoder.current_frame()` 读取。Track 级编码器延迟只在流的真正起点生效;原生 MP3/Vorbis 解码器在 seek 后消费其分组局部的修剪与重置预滚。

---

## 设计原则

### 实时优先

音频回调是硬实时边界。

在回调之前预备。通过有界、无锁的机制通信。

### 显式所有权

处理器、缓冲、生命周期状态与外部资源拥有显式所有权。

避免隐藏全局状态。

### 先测量再优化

性能与音频质量声明应当可复现。

基准在切实可行时针对公共 API 运行。

### 应用无关

核心拥有处理。

应用拥有播放、设备、UI、媒体库状态与运行时策略。

### 类型化失败

错误被显式表示,而非依赖实时代码中的日志、静默回退或基于 panic 的恢复。

### 可组合处理

高层流水线提供规范播放路径,而低层处理器 API 在应用需要自定义阶段顺序时仍然可用。

---

## 项目范围

`audio-engine-core` 刻意**不**提供:

* UI
* 播放列表管理
* 音乐库管理
* 音频设备所有权
* 应用生命周期
* 网络服务编排
* 播放器特定状态管理

它同样刻意不拥有设备管理(CPAL/WASAPI 输出流)、桌面 UI 或 Tauri 集成、播放队列逻辑、媒体库扫描、HTTP/WebSocket 服务路由、WebDAV 或网易云集成,以及应用运行时目录 —— 这些都留在 Lyne 应用 crate 中,且不为每个 Lyne 内部用例提供稳定兼容层。

本项目的目标更聚焦:

> **提供一个高品质、实时安全的音频处理基座,供应用在其上构建。**

---

## 适合谁

如果你符合以下情况,则很适合:

* 正在用 Rust 构建音乐播放器,需要一个处理核心垫底;
* 正在组装自定义实时音频流水线;
* 正在试验高品质 DSP(EQ、交叉馈送、饱和、卷积);
* 正在编写离线响度分析工具。

以下情况可能不适合:你需要完整播放器、高层播放 API,或音频设备抽象。

---

## 项目状态

稳定 `1.0.0` 发布:公共 API 完整文档化(编译期拒绝 `missing_docs`),由提交的公共面快照冻结,并由 CI 中的 SemVer 门禁守护 —— 任何破坏性变更都会在发布前使构建失败。要求 Rust 1.87+(Symphonia 0.6 本身要求 1.85;更高的 crate MSRV 反映本仓库中现有的 DSP 代码)。作为 Lyne 播放器的音频基础用于生产环境。破坏性变更按 `CONTRIBUTING.md` 中的策略保留给主版本号升级;重要变更记录在 `CHANGELOG.md`。

---

## 文档

* `docs/` — 架构与 API 文档
* `docs/quality.md` — 基准方法学与质量验证
* `docs/resampler-comparison.md` — 重采样器方法学与对比
* `docs/installation.md` — 各平台原生 SoXR 后端安装
* `examples/` — 可运行示例
* `benches/` — 公共 API 基准
* `tests/` — 集成与行为验证

API 文档:

```bash
cargo doc --open
```

---

## 稳定性

`audio-engine-core` 对其公共 API 遵循语义化版本控制。

`1.x` 系列面向想要稳定音频处理基座、同时保留自身播放架构控制权的应用。

破坏性 API 变更需要主版本号升级。

---

## 许可证

任选其一:

* Apache License, Version 2.0([LICENSE-APACHE](LICENSE-APACHE) 或 <http://www.apache.org/licenses/LICENSE-2.0>)
* MIT license([LICENSE-MIT](LICENSE-MIT) 或 <http://opensource.org/licenses/MIT>)

除非你明确另行声明,否则按 Apache-2.0 许可的定义,你有意为作品提交的任何贡献,均按上述双许可授权,不附加任何额外条款或条件。

### 原生依赖许可

启用默认 `soxr` 特性时,本 crate 链接 SoXR 原生库(libsoxr),其以 LGPL-2.1 分发。本 crate 的 Rust 源码为 MIT OR Apache-2.0,但静态链接 libsoxr 的二进制带有 LGPL-2.1 重链接义务。使用 `default-features = false` 与纯 Rust `rubato` 后端构建时不链接 libsoxr,不承担 LGPL 义务。详见 [NOTICE](NOTICE)。