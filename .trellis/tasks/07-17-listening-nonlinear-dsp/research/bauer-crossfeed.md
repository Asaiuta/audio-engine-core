# Bauer-style crossfeed 拓扑研究

## 现状与参考

当前 `src/processor/crossfeed.rs` 将对侧声道经过二阶 Butterworth **高通** 后相加，80 Hz crossfeed 实测约 `-46.81 dB`，2 kHz 约 `-9.18 dB`。这与扬声器串扰主要在低频更强、随频率升高衰减的 Bauer stereophonic-to-binaural 模型方向相反；现有 quality gate 也把这个错误方向固化成 oracle。

libbs2b 的参考实现使用：

* 对侧声道的单极点 low-pass crossfeed；
* 本声道的互补 high-boost 路径；
* 防止同相信号过载的全局 gain；
* 默认 700 Hz、4.5 dB low-frequency feed profile。

参考：

* <https://github.com/alexmarsev/libbs2b/blob/master/src/bs2b.c>
* <https://github.com/alexmarsev/libbs2b/blob/master/src/bs2b.h>

核心参考输出为：

```text
L_bauer = gain * (highboost(L) + lowpass(R))
R_bauer = gain * (highboost(R) + lowpass(L))
```

## 可行方案

### A. 只把 HPF 改成 LPF

实现最小，但 `L + mix*LPF(R)` 在同相低频会提升电平，且缺少 Bauer 参考的 direct-path compensation。

### B. 二阶 LPF + 静态归一化

频率方向正确且滚降更陡，但并非所标记的 Bauer/libbs2b 拓扑，参数和独立参考难以对齐。

### C. libbs2b 一阶 low-pass + high-boost + gain（采用）

按参考公式重建系数。保留仓库现有 `mix: 0..1` API，但明确它是 dry 到完整 Bauer profile 的 strength：

```text
out = dry + mix * (bauer_reference - dry)
```

`mix=0` 精确 bypass，`mix=1` 对齐 700 Hz / 4.5 dB reference profile；默认 0.30–0.35 是较轻的参考效果。这样既保留公共控制范围，又不把任意线性系数伪称为 feed dB。

## 参数连续性与实时约束

* `mix` 目标值按约 10 ms 逐样本线性 ramp，避免 callback block 边界的增益阶跃。
* cutoff 改变时计算一组 target coefficients，并在同一 ramp 时间内插值；一阶 pole 始终位于 `(0,1)`，端点间线性插值保持稳定。不得为普通 cutoff 更新清空 history。
* 真正的 sample-rate/新 stream 变更会 snap 新系数并 reset state；跨采样率本来就是生命周期边界。
* 所有 state 固定存于 struct，process 无锁、无分配、无日志。

## Oracle

* hard-left 正弦的 right-channel crossfeed：80 Hz 必须显著高于 2 kHz，并在更高频继续衰减。
* `mix=1` 的 steady-state direct/cross gains 与独立 libbs2b 公式匹配；同相 DC 总增益接近 unity。
* mix/cutoff 变更保留 history，首帧不出现 reset impulse，ramp 结束后到达新 steady state。
* stereo-only、mono/multichannel bypass、reset isolation、denormal 和零分配行为继续覆盖。

## 扩展边界

未来可以增加 Cmoy/JMeier profile 或直接暴露 feed dB，但这会改变配置模型。本任务只实现固定 4.5 dB reference profile + strength + cutoff，不新增 preset/public enum。
