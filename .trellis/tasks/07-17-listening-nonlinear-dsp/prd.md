# 修复 Listening 与 Nonlinear DSP

## Goal

修复 saturation、noise shaper 与 Bauer-labelled crossfeed 的 P1 音质/模型错误，并重建相应客观 oracle。

## Parent

* `../07-16-audio-core-quality-correctness/prd.md`

## Requirements

* Saturation threshold 两侧传递函数连续；必要时验证一阶斜率与 alias/THD 影响。
* Noise shaper 不对低于 -120 dBFS 的合法输入硬门控，处理 non-finite/越界输出的策略明确且有测试。
* Crossfeed 名称、拓扑和频率方向一致；若保留 Bauer-style，按低频交叉馈送参考重建系数与质量指标。
* 修改旧 quality gates 中固化错误高通行为的 oracle，不以旧阈值阻止正确模型。

## Acceptance Criteria

* [ ] Threshold 邻域最大跳变命中解析连续性阈值。
* [ ] 极低电平、silence、满幅和随机 stress 输出 finite，边界策略一致。
* [ ] Crossfeed 频响、通道串扰与参数切换连续性符合选定参考模型。
* [ ] Alias、noise-band 与 callback 性能证据更新且诚实分类。

## Dependencies

* `07-17-audio-quality-performance-gates`

