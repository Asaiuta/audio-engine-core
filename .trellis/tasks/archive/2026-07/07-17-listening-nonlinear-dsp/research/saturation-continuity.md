# Saturation threshold 连续性研究

## 现状与根因

检查 `src/processor/saturation.rs` 后确认，当前 full-band direct 与 oversampled 路径都在 `abs(input) > threshold` 时从 dry 直接切到 `F((1 + drive) * input)`：

* `F((1 + drive) * threshold)` 通常不等于 `threshold`，因此 threshold 两侧存在有限幅度跳变。
* full-band 路径只在 threshold 以上乘 `output_gain`，即使 waveshaper 本身连续，非 0 dB output gain 仍会制造第二个跳变。
* oversampled 内部同样调用硬门控 helper，插值样点穿越 threshold 时会产生同类宽带激励。
* high-pass exciter 路径始终应用 output gain，但仍复用硬门控 helper；三条路径的增益语义不一致。

## 可行方案

### A. 固定宽度 C1 soft knee（采用）

在 threshold 以上的有限 knee 区间内，用 smoothstep 将 identity 与现有 waveshaper 混合：

```text
u = clamp((abs(x) - threshold) / knee_width, 0, 1)
w = u²(3 - 2u)
wet = x + w * (F((1 + drive) * x) - x)
out = output_gain * (x + mix * (wet - x))
```

`w(0)=0` 且 `w'(0)=0`，所以 threshold 处值与一阶斜率都继承 identity；knee 结束后完整保留现有 Tape/Tube/Transistor 音色。固定 `0.05 FS` 的 knee 足以去除分支脉冲，又不会把整个动态范围变成缓慢 crossfade；knee 允许延伸到 0 dBFS 以上，因此 `threshold=1.0` 对合法的超满幅中间信号也保持连续。

优点：局部修复、保持既有 drive/type 语义、direct/oversampled/high-pass 可共用一个 helper、热路径只增加少量乘加。缺点：knee 内的曲线不再位精确兼容旧实现。

### B. 对 waveshaper 做常量平移

令 threshold 以上为 `threshold + F(g*x) - F(g*threshold)`。可以得到 C0 连续，但 threshold 右侧斜率仍通常不同于 1；Transistor 曲线在部分 drive/threshold 组合还可能出现负斜率。

### C. 在 threshold 以上重定义 residual-domain waveshaper

令 `e=abs(x)-threshold`，用 `F(g*e)/g` 后再加回 threshold。可以自然获得 C1，但会显著改变 threshold=0 时的 drive 增益与所有既有音色，属于更大的算法重设计。

## 仓库映射与验证

* direct、oversampled、high-pass 三条路径都调用相同 soft-knee transfer。
* output gain 改为 saturation 启用时对最终 wet/dry 结果一致应用；disabled 仍严格 bypass。
* unit tests 覆盖三种 saturation type、正负 threshold 邻域、非 0 dB output gain、direct/2x/4x finite/bounded。
* quality bench 记录 threshold 最大跳变和左右一阶斜率差；alias 指标继续比较 Direct 与 Oversampled4x，避免只修连续性却破坏抗混叠收益。

## 结论

采用方案 A。它修复缺陷机制且最少改变 threshold 之外的声音，是当前 API 与实时预算下风险最低的高质量方案。此次不重写 Transistor 基础曲线，也不替换现有 oversampling 架构；若客观 THD/alias 证据显示基础曲线需要重设计，另立任务。
