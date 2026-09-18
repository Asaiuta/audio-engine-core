//! Metadata and report output for the package-local accuracy harness.
use audio_engine_core::RESAMPLER_BACKEND_NAME;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BenchEnvironment {
    pub revision: String,
    pub dirty: Option<bool>,
    pub rustc: String,
    pub target: String,
    pub os: String,
    pub arch: String,
    pub cpu: String,
    pub profile: String,
    pub features: Vec<String>,
}

impl BenchEnvironment {
    pub fn capture() -> Self {
        let rustc_verbose =
            env_nonempty("AUDIO_BENCH_RUSTC_VERBOSE").or_else(|| command_output("rustc", &["-Vv"]));
        let rustc = env_nonempty("AUDIO_BENCH_RUSTC")
            .or_else(|| {
                rustc_verbose
                    .as_deref()
                    .and_then(|value| value.lines().next())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "unknown".to_string());
        let target = env_nonempty("AUDIO_BENCH_TARGET")
            .or_else(|| {
                rustc_verbose.as_deref().and_then(|value| {
                    value
                        .lines()
                        .find_map(|line| line.strip_prefix("host: ").map(str::to_string))
                })
            })
            .unwrap_or_else(|| "unknown".to_string());

        let revision = env_nonempty("AUDIO_BENCH_REVISION")
            .or_else(|| env_nonempty("GITHUB_SHA"))
            .or_else(|| command_output("git", &["rev-parse", "HEAD"]))
            .unwrap_or_else(|| "unknown".to_string());
        let dirty = env_nonempty("AUDIO_BENCH_DIRTY")
            .and_then(|value| parse_bool(&value))
            .or_else(git_dirty_state);
        let cpu = env_nonempty("AUDIO_BENCH_CPU")
            .or_else(|| env_nonempty("PROCESSOR_IDENTIFIER"))
            .or_else(linux_cpu_model)
            .or_else(|| command_output("sysctl", &["-n", "machdep.cpu.brand_string"]))
            .unwrap_or_else(|| "unknown".to_string());
        let profile = env_nonempty("AUDIO_BENCH_PROFILE").unwrap_or_else(|| {
            if cfg!(debug_assertions) {
                "debug".to_string()
            } else {
                "release".to_string()
            }
        });
        let mut features = Vec::new();
        if cfg!(feature = "http") {
            features.push("http".to_string());
        }
        // The compiled resampler backend changes the measured code, so record
        // it like a feature; cross-backend baseline comparisons must fail the
        // environment compatibility check.
        features.push(format!("resampler-{RESAMPLER_BACKEND_NAME}"));

        Self {
            revision,
            dirty,
            rustc,
            target,
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            cpu,
            profile,
            features,
        }
    }
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" => Some(true),
        "0" | "false" | "no" => Some(false),
        _ => None,
    }
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn git_dirty_state() -> Option<bool> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()?;
    output.status.success().then_some(!output.stdout.is_empty())
}

fn linux_cpu_model() -> Option<String> {
    if std::env::consts::OS != "linux" {
        return None;
    }
    let cpuinfo = fs::read_to_string("/proc/cpuinfo").ok()?;
    cpuinfo.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim() == "model name").then(|| value.trim().to_string())
    })
}

pub fn generated_unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}

pub fn write_json(path: &Path, value: &impl Serialize, report_name: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("failed to create '{}': {error}", parent.display()))?;
        }
    }
    let json = serde_json::to_string_pretty(value)
        .map_err(|error| format!("failed to serialize {report_name}: {error}"))?;
    fs::write(path, format!("{json}\n"))
        .map_err(|error| format!("failed to write '{}': {error}", path.display()))
}
