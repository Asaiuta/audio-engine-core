# Installation: Native SoXR Dependency

This page covers the native library setup required to build
`audio-engine-core`. For the Cargo feature flags (`http`, `loudness-db`), see
the [README](../README.md#installation--feature-flags).

## SoXR is required

The resampler depends on `soxr`, which requires the SoXR native library during
build/link. SoXR is part of the core crate today, so
`default-features = false` does **not** remove this native dependency.

Note that SoXR (libsoxr) is distributed under the LGPL-2.1; see the
[README license section](../README.md#license) and [NOTICE](../NOTICE) for the
licensing implications.

## Windows

On Windows, either install SoXR through vcpkg:

```powershell
git clone https://github.com/microsoft/vcpkg.git
cd vcpkg
.\bootstrap-vcpkg.bat
.\vcpkg install soxr:x64-windows-static-md
```

or through MSYS2/MinGW64, which is also the CI path:

```bash
pacman -S mingw-w64-x86_64-libsoxr mingw-w64-x86_64-pkgconf mingw-w64-x86_64-tools
```

For an MSVC Cargo build backed by the MSYS2 package, `build.rs` generates the
import library and deploys `libsoxr.dll` together with its matching MinGW
runtime DLLs beside Cargo binaries, tests, examples, and benchmarks. Direct
`cargo test`, `cargo run --example ...`, and `cargo bench ...` commands therefore
do not need a separate runtime `PATH` workaround after a successful build.

## Unix-like systems

On Unix-like systems, install SoXR through your system package manager and make
sure `pkg-config` can locate it.
