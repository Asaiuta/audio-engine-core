# Noise shaper 低电平与边界策略研究

## 参考与现状

* 当前 `src/processor/dsp.rs` 在 `abs(sample) < 1e-6`（约 -120 dBFS）时原样返回并清空 history。这个幅度门控使 dither 是否存在依赖输入电平，也让合法但不可由目标位深表示的低电平样本绕过量化。
* SoX `src/dither.c` / `src/dither.h` 的默认路径持续应用 dither；自动开关是显式 `-a` 选项，并标注 “use with caution”。它按源样本低位是否已满足目标 precision 判断，不使用固定 dBFS 幅度阈值。
* SoX 对整数输出执行目标 precision 的上下界 clipping。当前实现先 round 后直接乘 LSB，TPDF 在正满幅附近可能产生 `> 1.0` 的结果。

参考：

* <https://github.com/chirlu/sox/blob/master/src/dither.c>
* <https://github.com/chirlu/sox/blob/master/src/dither.h>

## 采用的处理契约

对 enabled 且 channel 有效的处理：

1. 所有 finite 输入（包括 exact zero 与低于 -120 dBFS 的值）都进入 TPDF + 目标位深量化；不做幅度门控。
2. 量化整数限制到有符号 N-bit 可表示区间 `[-2^(N-1), 2^(N-1)-1]`，因此输出落在 `[-1.0, 1.0-LSB]`。
3. error feedback 继续使用未裁剪量化误差并保留现有 `±2 LSB` 防御上限；输出 clipping 不被反馈成大误差。
4. NaN 或 ±Inf 输出 `0.0`，并只清空对应 channel 的 5-tap/9-tap history 与 ring head，防止非有限值污染后续整条流。
5. disabled 或非法 channel 保持现有透明 bypass 语义；显式 `reset()` 仍重置 history 与可复现 RNG seed。

exact digital silence 将产生标准 TPDF 量化噪声，这是位深缩减时正确且信号无关的行为。若未来产品需要 SoX 风格的 precision-aware auto-dither，应作为显式策略实现，不能恢复固定幅度门控。

## 参数更新

`NoiseShaperProcessor::sync_params` 当前在任何 snapshot generation 变化时都调用 `set_curve`，即使只改变 enabled/bits，也会无条件清空 history。实现应仅在 curve 实际变化时切换曲线；bits 和 enabled 分别更新，避免无关参数造成噪声状态突变。

## 验证

* -140 dBFS 常量不再原样穿透，输出全部位于目标量化网格。
* exact silence 会产生确定性的 TPDF 序列且长期 finite。
* 正负满幅、越界 finite、NaN/±Inf 与确定性 random stress 对所有 curve 都满足边界并不污染后续样本。
* 继续验证 ear-band 到 high-band 的噪声迁移方向；新增 low-level changed fraction 与 stress peak 门禁。
* callback benchmark 继续覆盖 24-bit TPDF hot path；unit allocation gate确保无新增堆分配。

## 结论

采用“持续 dither + signed target clamp + non-finite channel-local recovery”。该策略与标准位深缩减语义一致，也比固定 dBFS 门控更容易建立确定性 oracle。
