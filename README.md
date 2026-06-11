# audio-engine-core

Reusable decoder, DSP, loudness, resampling, and streaming pipeline primitives
extracted from the Lyne audio engine.

This crate is the app-agnostic core layer. It is intended for experiments and
integration work around high-quality local audio processing, not as a stable
1.0 SDK yet. The public API is versioned as `0.1.x` and may change while the
larger player continues to evolve.

## What Is Included

- Streaming decode helpers built on Symphonia.
- SoX VHQ resampling wrappers and streaming resampler utilities.
- DSP processors such as EQ, crossfeed, saturation, FFT convolution, dynamic
  loudness, volume smoothing, noise shaping, and spectrum analysis.
- EBU R128 loudness and true-peak measurement helpers.
- Lock-free DSP parameter snapshots and processor adapters for realtime audio
  callback integration.
- A small streaming pipeline/ring-buffer primitive.

## What Is Not Included

- Audio device ownership or CPAL/WASAPI output stream management.
- HTTP/WebSocket server routes.
- Desktop UI, Tauri integration, media-library scanning, playback queue logic,
  WebDAV, NetEase integration, or application runtime directories.
- A stable compatibility layer for every internal Lyne use case.

Those layers remain in the root Lyne application crate.

## Cargo Features

Both features below are enabled by default. Disable them with
`default-features = false` to drop the corresponding dependency.

- `http` (default): HTTP/HTTPS streaming decode via `reqwest`, including Range
  streaming and full-download fallback. With this off, `StreamingDecoder` only
  opens local files; passing an `http(s)://` path returns a decoder error, and
  the `reqwest` dependency and `NetworkError` type are not compiled.
- `loudness-db` (default): SQLite-backed loudness metadata persistence
  (`LoudnessDatabase`, `TrackLoudness`, `DatabaseStats`) via `rusqlite`. With
  this off, the EBU R128 measurement helpers (`LoudnessMeter`,
  `LoudnessNormalizer`, `TruePeakDetector`) still work; only the on-disk cache
  is removed.

DSP-only consumers can take a much smaller dependency tree:

```toml
[dependencies]
audio-engine-core = { version = "0.1", default-features = false }
```

## Native Dependency: SoXR

The resampler depends on `soxr`, which requires the SoXR native library during
build/link.

On Windows, vcpkg is the recommended path:

```powershell
git clone https://github.com/microsoft/vcpkg.git
cd vcpkg
.\bootstrap-vcpkg.bat
.\vcpkg install soxr:x64-windows-static-md
```

On MSYS2:

```bash
pacman -S mingw-w64-x86_64-soxr
```

On Unix-like systems, install SoXR through your system package manager and make
sure `pkg-config` can locate it.

## Quick Example

```rust
use audio_engine_core::{LoudnessMeter, StreamingDecoder};

fn analyze_file(path: &str) -> Result<f64, Box<dyn std::error::Error>> {
    let mut decoder = StreamingDecoder::open(path)?;
    let mut meter = LoudnessMeter::new(decoder.info.channels, decoder.info.sample_rate);

    while let Some(samples) = decoder.decode_next()? {
        meter.process(&samples);
    }

    Ok(meter.integrated_loudness())
}
```

## Runnable Examples

The `examples/` directory contains self-contained programs that need no audio
files and no optional features:

- `resample_sine` — streams a synthetic 48 kHz sine through the SoX VHQ
  resampler down to 44.1 kHz, demonstrating the chunked feed-then-flush pattern.
- `equalizer_curve` — runs a stereo buffer through the 10-band `Equalizer`.

```bash
cargo run --example resample_sine
cargo run --example equalizer_curve
```

## Realtime Notes

The crate exposes lock-free parameter containers and processor adapters used by
Lyne's realtime callback path. Keep allocations, locks, file I/O, logging, and
network I/O out of an audio callback. Allocate and configure processors before
entering the realtime path, then update parameters through the provided atomic
snapshot types.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.

### Native dependency licensing

This crate links the SoXR native library (libsoxr), which is distributed under
the LGPL-2.1. The Rust source in this crate is MIT OR Apache-2.0, but binaries
that statically link libsoxr carry LGPL-2.1 relinking obligations. See
[NOTICE](NOTICE) for details.
