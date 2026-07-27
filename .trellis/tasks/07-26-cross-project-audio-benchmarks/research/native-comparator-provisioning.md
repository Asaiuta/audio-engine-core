# Native Comparator Provisioning and API Evidence

Date: 2026-07-26

## Purpose

This record closes the research gap between the four-project phase-1 harness
and the required 11-project matrix. Every project below exposes an in-process,
stateful sample-rate converter that can accept the canonical stereo 44.1/48 kHz
rate pairs. Therefore none of these projects may be excluded merely because a
prebuilt library or convenient C ABI was initially absent.

The benchmark integration uses an explicit, benchmark-only C ABI shim where a
project exposes only a C++ API. Shim source, compiler command, pinned upstream
identity, and output hashes are evidence artifacts. Normal crate builds do not
link or search for these libraries.

## Pinned Sources and Binaries

| Project | Pinned identity | Local evidence |
| --- | --- | --- |
| FFmpeg libswresample | tag n8.0.1, commit `894da5ca7d742e4429ffb2af534fcda0103ef593` | Minimal shared release build containing only libavutil and libswresample |
| SpeexDSP | MSYS2 package `mingw-w64-x86_64-speexdsp-1.2.1-1` | Package signature verified with MSYS2 key `5F944B027F7FE2091985AA2EFA11531AA0AA7F57` |
| r8brain-free-src | commit `e71c31bf320f84210bb4bdcb57e296c39ce940f9` | Upstream source checkout and upstream Win64 DLL |
| zita-resampler | official author release 1.11.2 | `zita-resampler-1.11.2.tar.xz`, SHA-256 `AA5C54E696069AF26F3F1FED4A963113CC1237CDDFD57AE5842ABCB1ACD5492C` |
| WebRTC Audio Processing | release v1.3, commit `8e258a1933d405073c9e6465628a69ac7d2a1f13` | Source checkout plus signed MSYS2 package `1.3-2` |
| WDL | commit `96b770f7368f75b53756e0c8941ce3ecc8b6c29b` | Upstream source checkout |
| libresample | commit `7cb7f9c3f72d4e6774d964dc324af827192df7c3` | Upstream source checkout |

Pinned native hashes already available before shim construction:

| Artifact | SHA-256 |
| --- | --- |
| `swresample-6.dll` | `98FDB4D1788BD64C2282C7B363D5675F3844DD8BFCD8BBF27690C5595A63F0C7` |
| `avutil-60.dll` | `6D025CF39A586811EB4F8944C364DF507504CAE567F491A7A5A4FD17443BAA3E` |
| `libspeexdsp-1.dll` | `676DE283408C6A7C06221774BBF8150DCFE94668E0A249D67E81EDE17CC22A45` |
| upstream `r8bsrc.dll` | `82B38FA33C2BCBC3B400FE3404E9591D4711F9BD9BAA2A50E03E05C1982402B6` |
| `libwebrtc-audio-processing-1-3.dll` | `C116DFA387CF2DE96D2A247B3ADED9239F92AC0F9C8C180786AF0FBF03F6CD7E` |

Additional build-input identities enforced by the shim build script:

| Artifact | SHA-256 |
| --- | --- |
| FFmpeg installed include manifest | `9C7EF81AF2DA1EEA17A5C5EAA3A678BD72F5C0F70C99C4A22D4C064D43666AFA` |
| `libswresample.dll.a` | `3D5062996390328A34A3E4DE82CB1D97281622CB6AF722BFCCD25C0D503026A7` |
| `libavutil.dll.a` | `45F94EDDB106704164994C957BC05BD46288723D0AC97FEBA18C35AFBE2E4E7F` |
| `libspeexdsp.dll.a` | `0E1749DA0F497E21BD3A1D61F15A46B61CFBB825D5CD23318DF6C41AA4A71536` |
| `libwinpthread-1.dll` | `B0D84F7B6346CF835EF19ECC95991CDAA6272BB8AD6FEE43F446C07AA97FCBD9` |

The partial 3 MiB FFmpeg MSYS2 package download is not used as provenance or
build input. The FFmpeg source build above is the authoritative binary.

## Public API and Lifecycle Findings

### FFmpeg libswresample

`swr_alloc_set_opts2`, `swr_init`, `swr_convert`, `swr_get_delay`, and
`swr_free` provide an opaque streaming context, packed float/double lanes,
buffered progress, delay inspection, and documented null-input flushing. The
canonical benchmark can use packed interleaved stereo without a subprocess or
file I/O.

### SpeexDSP

`speex_resampler_init`, `speex_resampler_process_interleaved_float`,
`speex_resampler_get_output_latency`, `speex_resampler_reset_mem`, and
`speex_resampler_destroy` provide a complete f32 streaming lifecycle. Quality
10 is the library's highest public quality setting. Tail completion requires a
bounded zero-input-equivalent flush policy because the public API has no
end-of-input flag; the adapter must report that policy explicitly.

### r8brain-free-src

The upstream Win64 C API exposes `r8b_create`, `r8b_process`, `r8b_inlen`,
`r8b_clear`, and `r8b_delete`. It is a double-precision, linear-phase,
asynchronous block converter. `MaxInLen` makes the canonical 512-frame bound
explicit. Stereo is represented by one independent state per channel because
the upstream converter is mono.

### zita-resampler

`Resampler::setup`, public `inp_count`/`out_count` and data pointers,
`process`, `reset`, and `clear` provide interleaved float streaming with exact
consumed/produced counters. The public implementation is directly comparable;
its lack of a C ABI is build plumbing, not a technical exclusion.

### WebRTC

`webrtc::PushResampler<float>` and `PushSincResampler` provide stereo sinc
resampling for fixed 10 ms blocks. The canonical 512-frame caller schedule is
not itself a 10 ms schedule, so a preallocated staging adapter must accumulate
source frames, invoke native 441- or 480-frame blocks, and drain a final padded
block while trimming only output attributable to padding. This staging cost is
part of the measured adapter lane and must be disclosed.

### WDL

`WDL_Resampler` exposes `SetRates`, sinc `SetMode`, input-driven
`SetFeedMode`, `Prealloc`, `ResamplePrepare`, `ResampleOut`, `Reset`, and
`GetCurrentLatency`. The documented short-input `ResampleOut` call flushes
remaining valid samples. The API is double precision by default and directly
supports the canonical schedule through a benchmark-only C++ shim.

### libresample

The C API exposes `resample_open`, `resample_process`,
`resample_get_filter_width`, and `resample_close`. `resample_process` reports
input consumed and output generated and accepts `lastFlag` for complete-stream
drain. The high-quality f32 lane is technically comparable despite the
project's age.

## Toolchain

The native shim toolchain is MSYS2 MinGW-w64 GCC/G++ 15.2.0. It was installed
after the earlier FFmpeg-only build because missing compiler packages are a
non-terminal provisioning state, not evidence of infeasibility. Build commands
must use explicit source/include/library paths and release optimization; the
result report records each loaded shim's canonical path, SHA-256, and size.
The build script rejects dirty or untracked files in every pinned Git source
tree. It also verifies the FFmpeg installed include manifest
(`9C7EF81AF2DA1EEA17A5C5EAA3A678BD72F5C0F70C99C4A22D4C064D43666AFA`;
sorted relative-path, space, file-SHA-256 records encoded as UTF-8/LF), its
import libraries, runtime DLLs, the SpeexDSP link/runtime files, and the MinGW
pthread runtime before compilation.

## Coverage Consequence

All seven researched projects are `comparable` and therefore require measured
44.1-to-48 kHz and 48-to-44.1 kHz rows. A final coverage table containing
`skipped`, `unavailable`, a placeholder adapter, or a missing row remains
incomplete.

## Reproducible Shim Build Result

`benches/native/build_resampler_shims.ps1` validates every pinned source,
package hash, compiler version, and source revision before building. The final
post-lifecycle-fix build produced:

| Shim | SHA-256 | Bytes |
| --- | --- | ---: |
| FFmpeg libswresample | `955FF2955EA42DAD4E774BCF07E1AFB585778FA0ED03D5F3942FB64F67B6C82C` | 677931 |
| SpeexDSP | `5E29228A9A55A0E05097D8C9BA380AA6BA3D7079551167E2EA246A53A1042060` | 677571 |
| r8brain | `8AAC05F8FA830DD7A38760A9A902E2C0C34C3DA4A74E60EA8344FB49B52BB453` | 891260 |
| zita-resampler | `6299790821FB38517D1A3C6450DEEBC0AB04BBC79C76F57E93FE02FA2C604CC5` | 698762 |
| WebRTC | `7DA0744339FB944E60348AE3F015272CA0C79D8313A06FC564853791AE5B8E4C` | 726302 |
| WDL | `CBF5ADF903FED511BD2EF321830A90B397E6D022691E337C52A188B024B4B359` | 704111 |
| libresample | `23CABA6DA1423B2AE8D09D3F46F6992AAFF5A11830F893F7392198FE1E2E6C91` | 695625 |

The formal report hashes the loaded DLLs again and records every linked runtime
artifact. All binaries stay under ignored `target/benchmark-deps/` storage.
Every shim now exports ABI version 2. Its process and reset entry points are
`noexcept` boundaries that translate unexpected C++ exceptions into a named
shim error rather than allowing an exception to cross the Rust C ABI.

SpeexDSP reset deserves an explicit lifecycle note. In the tested 1.2.1
interleaved DLL, `speex_resampler_reset_mem` did not reproduce a fresh stereo
stream: right-channel history was visible at the first output frame after
reset. The benchmark shim therefore implements reset by constructing an
equivalent native state, swapping it only after success, and then destroying
the old state. Setup/reset timing includes that allocation and construction;
process and drain remain preallocated.
