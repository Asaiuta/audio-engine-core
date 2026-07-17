# Listening / Nonlinear DSP 质量与性能门禁范围

## 修复前基线（2026-07-17，本机 quick）

环境：Windows x86_64、Intel Family 6 Model 154、rustc 1.93.1、release/all-default-features。

* quality：16 gates 全部通过，但 crossfeed oracle 是旧高通方向；80 Hz `-46.81 dB`、2 kHz `-9.18 dB`。
* saturation：Direct 到 Oversampled4x 的 folded-harmonic alias reduction `16.56 dB`。
* noise shaping：最佳 shaped high-minus-ear-band advantage `34.79 dB`。
* callback active/no-convolver 512 frames：median `153.013 ns/sample`、`156.685 us/buffer`、deadline utilization `1.4689%`。
* callback active/IR256 512 frames：median `182.311 ns/sample`、`186.686 us/buffer`、deadline utilization `1.7502%`。

原始 JSON 保存在本机 ignored artifact：

* `target/bench-reports/listening-nonlinear-before-quality.json`
* `target/bench-reports/listening-nonlinear-before-callback.json`

## 门禁更新

### Saturation

* 新增 threshold 最大幅度跳变与左右一阶斜率差的 deterministic gate。
* 保留 alias reduction gate，避免 soft knee 以更严重 alias 为代价。

### Noise shaper

* 保留各 curve 的 noise-band 指标。
* 新增低于旧 -120 dBFS 阈值的 quantized/changed fraction。
* 新增 full-scale/越界/non-finite stress 的 finite 与 signed target peak 指标；结构性边界由 unit tests 做硬断言，bench 输出聚合证据。

### Crossfeed

* 删除 `high_band_min` 与 `low_vs_high_attenuation <= -12 dB` 这两个旧高通 oracle。
* 改为 low-band crossfeed level、`low - high` 正向分离、reference steady-state gain 与参数 history/ramp 连续性。

### Callback performance

* 复用 canonical `audio_callback_chain_perf` active scenarios；它已覆盖 Oversampled4x saturation、crossfeed 和 24-bit TPDF。
* 修复后使用同一进程配置和修复前 JSON 做 median regression comparison；超过 10% 才视为需要修复或明确批准。
* 不把跨机器绝对 ns 当 CI 硬阈值；CI 继续验证报告完整性、零分配测试和 deterministic quality gates。

## CI 范围

现有 `.github/workflows/ci.yml` 已执行：

```text
cargo bench --bench audio_quality_measurements -- --quick --enforce
cargo bench --bench audio_callback_chain_perf -- --quick --enforce
```

因此本任务只需扩充现有 report/gate schema，无需新增第二套 runner。schema 字段变化属于当前 `schema_version=1` 的向后不兼容扩展；若外部消费者要求稳定 schema，后续单独版本化。

## 修复后证据（2026-07-17，同机 quick）

* quality：23/23 synthetic gates 通过；EBU 外部语料仍按预期显示 2 个 skipped。
* saturation：threshold 最大跳变 `1.416e-6`，最大一阶斜率差 `3.610e-4`；Oversampled4x alias reduction `16.32 dB`，未牺牲原有抗混叠方向。
* Bauer crossfeed：80 Hz `-17.73 dB`、2 kHz `-27.27 dB`，低频比高频强 `9.54 dB`；DC reference 最大误差 `3.331e-16`；mix 首帧变化 `7.741e-6`，preserved-state reference delta `0`。
* noise shaper：-140 dBFS changed fraction `1.0`；exact-silence non-zero fraction `0.248535`（符合 TPDF）；全 curve overload/NaN/Inf stress 的 peak `1.0`、non-finite outputs `0`。
* callback 第一次修复后 512-frame active/no-convolver median `116.885 ns/sample`（修复前 `153.013`，`-23.61%`）；active/IR256 `124.369 ns/sample`（修复前 `182.311`，`-31.78%`）。复跑仍分别改善 `23.43%` 与 `29.90%`。
* 全 baseline 命令仅因 bypass 128/256/512 的约 `35–48 ns/buffer` 绝对差而非零退出；这些 case 只有 `0.215–0.762 ns/sample`、deadline utilization `<=0.0074%`，且 64-frame bypass 反而改善。两次 active DSP 的全部 8 个 case 均通过 10% gate，因此将此记录为 sub-nanosecond measurement-floor 限制，不把它误报为算法热路径回退。

## Windows SoXR runtime 部署修复

最终复跑首次直接执行 quality bench 时返回 `STATUS_DLL_NOT_FOUND`。根因不是 benchmark 或 DSP：旧 `build.rs` 从 `PKG_CONFIG_PATH=<prefix>/lib/pkgconfig` 只向上退一级，错误查找 `<prefix>/lib/bin`；它随后可能从 Cargo 输出目录重新拾取一份孤立的 `libsoxr.dll`。旧部署又只复制到 `target/<profile>`，没有覆盖位于 `target/<profile>/deps` 的 test/bench executable，也没有复制 MSYS2 SoXR 的同源 `libgomp-1.dll -> libgcc_s_seh-1.dll -> libwinpthread-1.dll` 运行时闭包。

修复后，构建脚本从 pkg-config 目录正确解析 `<prefix>/bin`，优先使用同一 MSYS2 installation，内容校验后把 SoXR 与三项 MinGW runtime 部署到 profile root、`deps` 和 `examples`，并把这些外部 DLL 注册为 Cargo rerun inputs。独立回归测试覆盖路径解析、精确闭包（不复制无关 DLL）、三个 executable directory 和 stale destination 刷新。

不设置临时 PATH 的直接验证：

```text
cargo bench --bench audio_quality_measurements -- --quick --enforce --out ...
23/23 gates passed, 2 external-corpus gates skipped, exit 0

cargo bench --bench audio_callback_chain_perf -- --quick --enforce --out ...
12/12 report/work cases valid, exit 0
```

第二条命令仅用于证明 callback benchmark 可直接启动及报告完整，不替换上面的同机性能基线结论；紧随 release rebuild 的单次 timing 明显受系统负载影响，且没有 supplied baseline，因此仍按既有协议保持 report-only。
