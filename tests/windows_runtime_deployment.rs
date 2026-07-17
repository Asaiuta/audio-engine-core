#[path = "../build/windows_runtime.rs"]
mod windows_runtime;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

const EXPECTED_DLLS: [(&str, &[u8]); 4] = [
    ("libsoxr.dll", b"soxr-runtime"),
    ("libgomp-1.dll", b"openmp-runtime"),
    ("libgcc_s_seh-1.dll", b"gcc-runtime"),
    ("libwinpthread-1.dll", b"pthread-runtime"),
];

#[test]
fn resolves_pkg_config_directory_to_the_matching_prefix_bin_directory() {
    let pkg_config_dir = Path::new("msys2")
        .join("mingw64")
        .join("lib")
        .join("pkgconfig");

    assert_eq!(
        windows_runtime::soxr_dll_candidates_from_pkg_config_dir(&pkg_config_dir),
        vec![
            Path::new("msys2")
                .join("mingw64")
                .join("bin")
                .join("libsoxr.dll"),
            Path::new("msys2")
                .join("mingw64")
                .join("bin")
                .join("soxr.dll"),
        ]
    );
}

#[test]
fn deploys_matching_runtime_closure_to_every_cargo_executable_directory() {
    let fixture = RuntimeFixture::new();
    fs::write(fixture.source_dir.join("unrelated.dll"), b"unrelated").unwrap();

    windows_runtime::deploy_runtime_dlls(&fixture.soxr_dll, &fixture.out_dir).unwrap();

    for executable_dir in fixture.executable_dirs() {
        for (name, expected) in EXPECTED_DLLS {
            assert_eq!(fs::read(executable_dir.join(name)).unwrap(), expected);
        }
        assert!(!executable_dir.join("unrelated.dll").exists());
    }
}

#[test]
fn refreshes_stale_deployed_runtime_dlls() {
    let fixture = RuntimeFixture::new();
    for executable_dir in fixture.executable_dirs() {
        fs::create_dir_all(&executable_dir).unwrap();
        for (name, _) in EXPECTED_DLLS {
            fs::write(executable_dir.join(name), b"stale").unwrap();
        }
    }

    windows_runtime::deploy_runtime_dlls(&fixture.soxr_dll, &fixture.out_dir).unwrap();

    for executable_dir in fixture.executable_dirs() {
        for (name, expected) in EXPECTED_DLLS {
            assert_eq!(fs::read(executable_dir.join(name)).unwrap(), expected);
        }
    }
}

struct RuntimeFixture {
    root: PathBuf,
    source_dir: PathBuf,
    out_dir: PathBuf,
    profile_dir: PathBuf,
    soxr_dll: PathBuf,
}

impl RuntimeFixture {
    fn new() -> Self {
        let fixture_id = NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "audio-engine-core-runtime-deployment-{}-{fixture_id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);

        let source_dir = root.join("msys2").join("mingw64").join("bin");
        let profile_dir = root.join("target").join("release");
        let out_dir = profile_dir
            .join("build")
            .join("audio-engine-core-fixture")
            .join("out");
        fs::create_dir_all(&source_dir).unwrap();
        fs::create_dir_all(&out_dir).unwrap();

        for (name, contents) in EXPECTED_DLLS {
            fs::write(source_dir.join(name), contents).unwrap();
        }

        let soxr_dll = source_dir.join("libsoxr.dll");
        Self {
            root,
            source_dir,
            out_dir,
            profile_dir,
            soxr_dll,
        }
    }

    fn executable_dirs(&self) -> impl Iterator<Item = PathBuf> + '_ {
        [Path::new(""), Path::new("deps"), Path::new("examples")]
            .into_iter()
            .map(|subdirectory| self.profile_dir.join(subdirectory))
    }
}

impl Drop for RuntimeFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
