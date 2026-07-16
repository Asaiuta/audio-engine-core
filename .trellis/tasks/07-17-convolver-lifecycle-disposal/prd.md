# 修复 Convolver 生命周期与资源回收

## Goal

修复 canonical builder 隐藏 convolver disposal slot 导致的长期 kernel adoption 阻塞，并验证统一 streaming 生命周期下卷积尾部和资源回收。

## Parent

* `../07-16-audio-core-quality-correctness/prd.md`

## Requirements

* Canonical builder 显式接入 disposal/reclamation 通道，不让 audio thread 负责重资源析构。
* Kernel swap/adoption 在长期运行和快速连续更新下有界、无锁、无热路径分配。
* Convolver 准确报告 latency 与有限 IR tail，并参与 offline finalize 传播。
* Backpressure 或 disposal 堆积必须可观测并有明确失败/降级策略。

## Acceptance Criteria

* [ ] 长时间 kernel swap stress 中新 kernel 持续被采纳，无永久阻塞或未界定增长。
* [ ] Audio callback 不执行昂贵析构，不新增锁/分配。
* [ ] Impulse tail 长度和内容与直接卷积 reference 一致。
* [ ] Convolver 与 callback/FIR 基准无未经批准的显著回退。

## Dependencies

* `07-17-variable-io-offline-finalize`
* `07-17-audio-quality-performance-gates`

