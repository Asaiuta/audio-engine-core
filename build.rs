use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[allow(dead_code)]
fn main() {
    run_build_script();
}

pub fn run_build_script() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=PKG_CONFIG_PATH");
    println!("cargo:rerun-if-env-changed=VCPKG_ROOT");

    if env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "windows" {
        return;
    }

    if vcpkg::find_package("soxr").is_ok() {
        return;
    }

    match pkg_config::probe_library("soxr") {
        Ok(library) => {
            if env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default() == "msvc" {
                if let Err(error) = ensure_msvc_import_lib(&library) {
                    println!(
                        "cargo:warning=Found soxr via pkg-config but failed to prepare an MSVC import library: {error}"
                    );
                }
            }
        }
        Err(error) => {
            println!("cargo:warning=Both vcpkg and pkg-config failed to find soxr: {error}");
        }
    }
}

fn ensure_msvc_import_lib(library: &pkg_config::Library) -> Result<(), String> {
    let dll_path = find_soxr_dll(library).ok_or_else(|| {
        "unable to locate libsoxr.dll next to the pkg-config library path".to_string()
    })?;

    let out_dir = PathBuf::from(env::var("OUT_DIR").map_err(|error| error.to_string())?);
    let def_file_name = format!(
        "{}.def",
        dll_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| "soxr dll path is missing a valid file stem".to_string())?
    );
    let def_path = out_dir.join(def_file_name);
    let import_lib_path = out_dir.join("soxr.lib");

    if !import_lib_path.exists() {
        let gendef_path = find_gendef(&dll_path)
            .ok_or_else(|| "unable to find gendef.exe to generate soxr.def".to_string())?;
        let lib_exe_path = find_lib_exe().ok_or_else(|| {
            "unable to find Visual Studio lib.exe to generate soxr.lib".to_string()
        })?;

        run(
            Command::new(gendef_path)
                .arg(&dll_path)
                .current_dir(&out_dir),
            "gendef",
        )?;
        run(
            Command::new(lib_exe_path)
                .arg(format!("/DEF:{}", def_path.display()))
                .arg(format!("/OUT:{}", import_lib_path.display()))
                .arg("/MACHINE:X64"),
            "lib.exe",
        )?;
    }

    println!("cargo:rustc-link-search=native={}", out_dir.display());
    copy_runtime_dll(&dll_path, &out_dir)?;

    Ok(())
}

fn find_soxr_dll(library: &pkg_config::Library) -> Option<PathBuf> {
    for link_path in &library.link_paths {
        let candidates = [
            link_path.join("soxr.lib"),
            link_path.join("libsoxr.lib"),
            link_path.join("libsoxr.dll.a"),
            link_path.join("libsoxr.a"),
        ];

        if candidates.iter().any(|path| path.exists()) {
            let bin_dir = link_path.parent()?.join("bin");
            for dll_name in ["libsoxr.dll", "soxr.dll"] {
                let dll_path = bin_dir.join(dll_name);
                if dll_path.exists() {
                    return Some(dll_path);
                }
            }
        }
    }

    if let Some(dll_path) = env::var_os("PKG_CONFIG_PATH").and_then(|paths| {
        env::split_paths(&paths)
            .flat_map(|dir| {
                let mut candidates = vec![dir.join("..").join("bin").join("libsoxr.dll")];
                candidates.push(dir.join("..").join("bin").join("soxr.dll"));
                candidates
            })
            .find(|path| path.exists())
    }) {
        return Some(dll_path);
    }

    if let Some(dll_path) = path_candidates("libsoxr.dll")
        .into_iter()
        .chain(path_candidates("soxr.dll"))
        .find(|path| path.exists())
    {
        return Some(dll_path);
    }

    if let Some(user_profile) = env::var_os("USERPROFILE") {
        let scoop_msys2 = PathBuf::from(user_profile)
            .join("scoop")
            .join("apps")
            .join("msys2");
        if let Ok(entries) = fs::read_dir(scoop_msys2) {
            for entry in entries.filter_map(Result::ok) {
                let bin_dir = entry.path().join("mingw64").join("bin");
                for dll_name in ["libsoxr.dll", "soxr.dll"] {
                    let dll_path = bin_dir.join(dll_name);
                    if dll_path.exists() {
                        return Some(dll_path);
                    }
                }
            }
        }
    }

    None
}

fn find_gendef(dll_path: &Path) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(bin_dir) = dll_path.parent() {
        candidates.push(bin_dir.join("gendef.exe"));
    }
    candidates.extend(path_candidates("gendef.exe"));
    candidates.into_iter().find(|path| path.exists())
}

fn find_lib_exe() -> Option<PathBuf> {
    let mut candidates = path_candidates("lib.exe");

    if let Some(vswhere) = find_vswhere() {
        let output = Command::new(vswhere)
            .args([
                "-latest",
                "-products",
                "*",
                "-requires",
                "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
                "-property",
                "installationPath",
            ])
            .output()
            .ok()?;

        if output.status.success() {
            let install_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !install_path.is_empty() {
                let tools_root = Path::new(&install_path)
                    .join("VC")
                    .join("Tools")
                    .join("MSVC");
                if let Ok(entries) = fs::read_dir(tools_root) {
                    let mut versions = entries
                        .filter_map(Result::ok)
                        .map(|entry| entry.path())
                        .filter(|path| path.is_dir())
                        .collect::<Vec<_>>();
                    versions.sort();
                    versions.reverse();
                    for version_dir in versions {
                        candidates.push(
                            version_dir
                                .join("bin")
                                .join("HostX64")
                                .join("x64")
                                .join("lib.exe"),
                        );
                    }
                }
            }
        }
    }

    candidates.into_iter().find(|path| path.exists())
}

fn find_vswhere() -> Option<PathBuf> {
    let explicit =
        PathBuf::from(r"C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe");
    if explicit.exists() {
        return Some(explicit);
    }
    path_candidates("vswhere.exe")
        .into_iter()
        .find(|path| path.exists())
}

fn path_candidates(name: &str) -> Vec<PathBuf> {
    env::var_os("PATH")
        .map(|path| env::split_paths(&path).map(|dir| dir.join(name)).collect())
        .unwrap_or_default()
}

fn copy_runtime_dll(dll_path: &Path, out_dir: &Path) -> Result<(), String> {
    let profile_dir = out_dir
        .ancestors()
        .nth(3)
        .ok_or_else(|| "unable to resolve Cargo profile output directory".to_string())?;
    let destination = profile_dir.join(
        dll_path
            .file_name()
            .ok_or_else(|| "soxr dll path is missing a file name".to_string())?,
    );

    if destination.exists() {
        return Ok(());
    }

    fs::copy(dll_path, &destination).map_err(|error| {
        format!(
            "failed to copy {} to {}: {error}",
            dll_path.display(),
            destination.display()
        )
    })?;

    Ok(())
}

fn run(command: &mut Command, name: &str) -> Result<(), String> {
    let output = command
        .output()
        .map_err(|error| format!("failed to run {name}: {error}"))?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Err(format!(
        "{name} exited with {}. stdout: {} stderr: {}",
        output.status, stdout, stderr
    ))
}
