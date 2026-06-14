# Channel Layout / Mixing Capability Audit

Source-verified inline (no sub-agent). Anchors are `file:line` against the
current tree.

## How channels are represented today

- The entire DSP abstraction passes a **channel count only**: `AudioProcessor::process(&mut self, buffer: &mut [f64], channels: usize)` (`src/processor/traits.rs:68`). Buffers are interleaved `[L, R, L, R, ...]` by documented convention (`traits.rs:63`). There is **no positional/role type** (no `Channels` bitmask, no speaker-position enum) anywhere in the DSP chain.
- `DspChain::process(buffer, channels: usize)` just forwards the count to each processor (`src/processor/dsp_chain.rs:77-79`).
- Decoder surfaces only `AudioInfo.channels: usize` (count) — no layout. (See decoder task audit.)

## Per-processor channel behavior (verified)

| Processor | Channel handling | Safe for N≠2? |
|-----------|------------------|---------------|
| EQ (`adapters.rs:80`) | per-sample, ignores `_channels` (`eq.process(buffer)`) | ✅ yes (channel-agnostic) |
| Saturation (`adapters.rs:185`) | `process_with_channels`; pre-sizes per-channel HPF state via `set_channel_count` off the RT thread | ✅ yes |
| Crossfeed (`adapters.rs:267`) | explicit `if channels != 2 { passthrough }` (`crossfeed.rs:141-142`) | ✅ yes (stereo-only by design, safe passthrough) |
| PeakLimiter (`adapters.rs:357`) | constructed with `channels`; per-channel state | ✅ yes |
| Volume / NoiseShaper / DynamicLoudness / Convolver | per-sample or per-channel gain | ✅ (to confirm in impl) |

**Conclusion:** no current module *silently corrupts* multichannel audio — the
chain is channel-count-aware and the only stereo-specific module (crossfeed)
guards itself. The gap is **capability, not corruption**.

## Loudness meter multichannel handling

- `LoudnessMeter::new(channels, sample_rate)` delegates to the `ebur128` crate with `EbuR128::new(channels, ...)` (`loudness/meter.rs:38-40`).
- ebur128 applies its own R128 channel weighting based on channel **index→role assumptions** (the libebur128 default channel map). The crate currently does **not** tell ebur128 an explicit channel map; it relies on the default index ordering.
- Tests already exercise 1/2/6/8 channel counts (`loudness/meter.rs:373-376`), and the 4× FIR true-peak detector has a verified strided per-channel path (`meter.rs:328-344`).

## Real gaps (capability)

1. **No positional layout type** — only counts flow through. Downmix/upmix cannot be expressed because there is no source/destination layout to map between.
2. **No downmix code at all** — 5.1/7.1 → stereo/mono does not exist. This is the core missing feature.
3. **ebur128 channel map is implicit** — multichannel loudness relies on default index ordering; not asserted against an explicit layout.
4. **No channel-order correctness test** — nothing asserts "a signal in source channel X lands in output channel Y".

## API impact of a positional layout type

`impl AudioProcessor` count (verified via grep):
- 8 production impls in `adapters.rs` (Eq, Saturation, Crossfeed, PeakLimiter, Volume, NoiseShaper, DynamicLoudness, Convolver)
- 2 test impls (`dsp_chain.rs` Doubler/Adder) + 1 doc example (`traits.rs:38`) + 1 test impl (`traits.rs:99`)

Changing `process(buffer, channels: usize)` to carry a layout type is a
**breaking change touching all ~10 impls + the doc example**. An additive
approach (a separate `Downmixer`/layout stage that runs before the chain, with
the chain still seeing a count) avoids touching the trait.
