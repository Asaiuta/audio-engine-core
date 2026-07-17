use std::fs;
use std::path::{Path, PathBuf};

const CARGO_EXECUTABLE_SUBDIRECTORIES: [&str; 3] = ["", "deps", "examples"];
const SOXR_DLL_NAMES: [&str; 2] = ["libsoxr.dll", "soxr.dll"];

// The supported MSYS2 libsoxr package uses OpenMP and the MinGW runtime DLLs
// below. Keep them beside the exact libsoxr build they came from so Windows
// never resolves an ABI-incompatible copy from another toolchain on PATH.
const MINGW_RUNTIME_DLL_NAMES: [&str; 3] =
    ["libgomp-1.dll", "libgcc_s_seh-1.dll", "libwinpthread-1.dll"];

pub(crate) fn soxr_dll_candidates_from_pkg_config_dir(pkg_config_dir: &Path) -> Vec<PathBuf> {
    let Some(prefix_dir) = pkg_config_dir.ancestors().nth(2) else {
        return Vec::new();
    };
    let bin_dir = prefix_dir.join("bin");
    SOXR_DLL_NAMES
        .into_iter()
        .map(|name| bin_dir.join(name))
        .collect()
}

pub(crate) fn deploy_runtime_dlls(soxr_dll: &Path, out_dir: &Path) -> Result<(), String> {
    let profile_dir = cargo_profile_dir(out_dir)?;
    let runtime_dlls = runtime_dlls_beside(soxr_dll)?;

    for source in &runtime_dlls {
        println!("cargo:rerun-if-changed={}", source.display());
    }

    for subdirectory in CARGO_EXECUTABLE_SUBDIRECTORIES {
        let executable_dir = profile_dir.join(subdirectory);
        fs::create_dir_all(&executable_dir).map_err(|error| {
            format!(
                "failed to create Cargo executable directory {}: {error}",
                executable_dir.display()
            )
        })?;

        for source in &runtime_dlls {
            let file_name = source.file_name().ok_or_else(|| {
                format!("runtime DLL path has no file name: {}", source.display())
            })?;
            copy_if_changed(source, &executable_dir.join(file_name))?;
        }
    }

    Ok(())
}

fn cargo_profile_dir(out_dir: &Path) -> Result<PathBuf, String> {
    out_dir
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            format!(
                "unable to resolve Cargo profile output directory from {}",
                out_dir.display()
            )
        })
}

fn runtime_dlls_beside(soxr_dll: &Path) -> Result<Vec<PathBuf>, String> {
    if !soxr_dll.is_file() {
        return Err(format!(
            "SoXR runtime DLL does not exist: {}",
            soxr_dll.display()
        ));
    }

    let source_dir = soxr_dll.parent().ok_or_else(|| {
        format!(
            "SoXR runtime DLL has no parent directory: {}",
            soxr_dll.display()
        )
    })?;
    let mut runtime_dlls = vec![soxr_dll.to_path_buf()];

    for name in MINGW_RUNTIME_DLL_NAMES {
        let candidate = source_dir.join(name);
        if candidate.is_file() {
            runtime_dlls.push(candidate);
        }
    }

    Ok(runtime_dlls)
}

fn copy_if_changed(source: &Path, destination: &Path) -> Result<(), String> {
    if files_equal(source, destination)? {
        return Ok(());
    }

    fs::copy(source, destination).map_err(|error| {
        format!(
            "failed to copy runtime DLL {} to {}: {error}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}

fn files_equal(source: &Path, destination: &Path) -> Result<bool, String> {
    if !destination.is_file() {
        return Ok(false);
    }

    let source_bytes = fs::read(source)
        .map_err(|error| format!("failed to read runtime DLL {}: {error}", source.display()))?;
    let destination_bytes = fs::read(destination).map_err(|error| {
        format!(
            "failed to read deployed runtime DLL {}: {error}",
            destination.display()
        )
    })?;
    Ok(source_bytes == destination_bytes)
}
