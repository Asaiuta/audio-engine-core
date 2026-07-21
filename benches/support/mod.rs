use std::collections::BTreeMap;
use std::fmt::Debug;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use audio_engine_core::processor::RESAMPLER_BACKEND_NAME;

pub const REPORT_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_MAX_MEDIAN_REGRESSION_PCT: f64 = 10.0;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchMode {
    Quick,
    Full,
    Heavy,
}

impl BenchMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Quick => "quick",
            Self::Full => "full",
            Self::Heavy => "heavy",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PerfArgs {
    pub mode: BenchMode,
    pub enforce: bool,
    pub out: Option<PathBuf>,
    pub baseline: Option<PathBuf>,
    pub max_median_regression_pct: f64,
    pub help: bool,
}

impl PerfArgs {
    pub fn parse(argv: Vec<String>) -> Result<Self, String> {
        let mut quick = false;
        let mut heavy = false;
        let mut enforce = false;
        let mut out = None;
        let mut baseline = None;
        let mut max_median_regression_pct = DEFAULT_MAX_MEDIAN_REGRESSION_PCT;
        let mut help = false;
        let mut index = 0usize;

        while index < argv.len() {
            let arg = &argv[index];
            match arg.as_str() {
                "--quick" => quick = true,
                "--heavy" => heavy = true,
                "--enforce" => enforce = true,
                "--bench" => {}
                "--help" | "-h" => help = true,
                "--out" => {
                    out = Some(PathBuf::from(next_value(&argv, &mut index, "--out")?));
                }
                "--baseline" => {
                    baseline = Some(PathBuf::from(next_value(&argv, &mut index, "--baseline")?));
                }
                "--max-median-regression-pct" => {
                    max_median_regression_pct = parse_nonnegative_finite(
                        next_value(&argv, &mut index, "--max-median-regression-pct")?,
                        "--max-median-regression-pct",
                    )?;
                }
                _ => {
                    if let Some(value) = arg.strip_prefix("--out=") {
                        out = Some(PathBuf::from(require_nonempty(value, "--out")?));
                    } else if let Some(value) = arg.strip_prefix("--baseline=") {
                        baseline = Some(PathBuf::from(require_nonempty(value, "--baseline")?));
                    } else if let Some(value) = arg.strip_prefix("--max-median-regression-pct=") {
                        max_median_regression_pct =
                            parse_nonnegative_finite(value, "--max-median-regression-pct")?;
                    } else {
                        return Err(format!("unknown argument: {arg}"));
                    }
                }
            }
            index += 1;
        }

        if quick && heavy {
            return Err("--quick and --heavy are mutually exclusive".to_string());
        }

        Ok(Self {
            mode: if quick {
                BenchMode::Quick
            } else if heavy {
                BenchMode::Heavy
            } else {
                BenchMode::Full
            },
            enforce,
            out,
            baseline,
            max_median_regression_pct,
            help,
        })
    }
}

fn next_value<'a>(argv: &'a [String], index: &mut usize, option: &str) -> Result<&'a str, String> {
    let value = argv
        .get(*index + 1)
        .ok_or_else(|| format!("{option} requires a value"))?;
    *index += 1;
    require_nonempty(value, option)
}

fn require_nonempty<'a>(value: &'a str, option: &str) -> Result<&'a str, String> {
    if value.is_empty() {
        Err(format!("{option} requires a non-empty value"))
    } else {
        Ok(value)
    }
}

fn parse_nonnegative_finite(value: &str, option: &str) -> Result<f64, String> {
    let parsed = value
        .parse::<f64>()
        .map_err(|_| format!("{option} requires a number, got '{value}'"))?;
    if !parsed.is_finite() || parsed < 0.0 {
        return Err(format!(
            "{option} requires a finite non-negative number, got '{value}'"
        ));
    }
    Ok(parsed)
}

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
        if cfg!(feature = "loudness-db") {
            features.push("loudness-db".to_string());
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

    pub fn compatibility_issues(&self, baseline: &Self) -> Vec<String> {
        let mut issues = Vec::new();
        compare_field("rustc", &self.rustc, &baseline.rustc, &mut issues);
        compare_field("target", &self.target, &baseline.target, &mut issues);
        compare_field("os", &self.os, &baseline.os, &mut issues);
        compare_field("arch", &self.arch, &baseline.arch, &mut issues);
        compare_field("cpu", &self.cpu, &baseline.cpu, &mut issues);
        compare_field("profile", &self.profile, &baseline.profile, &mut issues);
        if self.features != baseline.features {
            issues.push(format!(
                "features differ: candidate {:?}, baseline {:?}",
                self.features, baseline.features
            ));
        }
        issues
    }
}

pub struct PerformanceReportIdentity<'a, T: ?Sized> {
    pub schema_version: u32,
    pub probe: &'a str,
    pub mode: BenchMode,
    pub environment: &'a BenchEnvironment,
    pub conditions: &'a T,
}

pub fn validate_performance_baseline<T: Debug + PartialEq + ?Sized>(
    report_name: &str,
    candidate: PerformanceReportIdentity<'_, T>,
    baseline: PerformanceReportIdentity<'_, T>,
) -> Result<(), String> {
    if baseline.schema_version != candidate.schema_version {
        return Err(format!(
            "{report_name} baseline schema differs: candidate {}, baseline {}",
            candidate.schema_version, baseline.schema_version
        ));
    }
    if baseline.probe != candidate.probe {
        return Err(format!(
            "{report_name} baseline probe differs: candidate '{}', baseline '{}'",
            candidate.probe, baseline.probe
        ));
    }
    if baseline.mode != candidate.mode {
        return Err(format!(
            "{report_name} baseline mode differs: candidate '{}', baseline '{}'",
            candidate.mode.as_str(),
            baseline.mode.as_str()
        ));
    }
    if baseline.conditions != candidate.conditions {
        return Err(format!(
            "{report_name} baseline conditions differ: candidate {:?}, baseline {:?}",
            candidate.conditions, baseline.conditions
        ));
    }
    let issues = candidate
        .environment
        .compatibility_issues(baseline.environment);
    if !issues.is_empty() {
        return Err(format!(
            "{report_name} baseline environment is incompatible: {}",
            issues.join("; ")
        ));
    }
    Ok(())
}

fn compare_field(name: &str, candidate: &str, baseline: &str, issues: &mut Vec<String>) {
    if candidate == "unknown" || baseline == "unknown" {
        issues.push(format!(
            "{name} is unavailable: candidate '{candidate}', baseline '{baseline}'"
        ));
    } else if candidate != baseline {
        issues.push(format!(
            "{name} differs: candidate '{candidate}', baseline '{baseline}'"
        ));
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

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TrialDistribution {
    pub samples: Vec<f64>,
    pub min: f64,
    pub median: f64,
    pub p95: f64,
    pub max: f64,
}

pub fn summarize_trials(samples: Vec<f64>) -> Result<TrialDistribution, String> {
    if samples.is_empty() {
        return Err("trial distribution requires at least one sample".to_string());
    }
    if let Some((index, value)) = samples
        .iter()
        .copied()
        .enumerate()
        .find(|(_, value)| !value.is_finite() || *value <= 0.0)
    {
        return Err(format!(
            "trial sample {index} must be finite and positive, got {value}"
        ));
    }

    let mut sorted = samples.clone();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    let median = if sorted.len().is_multiple_of(2) {
        (sorted[middle - 1] + sorted[middle]) * 0.5
    } else {
        sorted[middle]
    };
    let p95_rank = ((sorted.len() as f64 * 0.95).ceil() as usize).max(1);

    Ok(TrialDistribution {
        min: sorted[0],
        median,
        p95: sorted[p95_rank - 1],
        max: sorted[sorted.len() - 1],
        samples,
    })
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RegressionComparison {
    pub case_key: String,
    pub baseline_median: f64,
    pub candidate_median: f64,
    pub regression_pct: f64,
    pub threshold_pct: f64,
    pub passed: bool,
}

pub fn compare_case_medians(
    candidate: impl IntoIterator<Item = (String, f64)>,
    baseline: impl IntoIterator<Item = (String, f64)>,
    threshold_pct: f64,
) -> Result<Vec<RegressionComparison>, String> {
    if !threshold_pct.is_finite() || threshold_pct < 0.0 {
        return Err(format!(
            "median regression threshold must be finite and non-negative, got {threshold_pct}"
        ));
    }
    let candidate = unique_case_map(candidate, "candidate")?;
    let baseline = unique_case_map(baseline, "baseline")?;
    let missing_in_candidate = baseline
        .keys()
        .filter(|key| !candidate.contains_key(*key))
        .cloned()
        .collect::<Vec<_>>();
    let missing_in_baseline = candidate
        .keys()
        .filter(|key| !baseline.contains_key(*key))
        .cloned()
        .collect::<Vec<_>>();
    if !missing_in_candidate.is_empty() || !missing_in_baseline.is_empty() {
        return Err(format!(
            "case sets differ: missing in candidate {:?}; missing in baseline {:?}",
            missing_in_candidate, missing_in_baseline
        ));
    }

    candidate
        .into_iter()
        .map(|(case_key, candidate_median)| {
            let baseline_median = baseline[&case_key];
            validate_median(&case_key, "candidate", candidate_median)?;
            validate_median(&case_key, "baseline", baseline_median)?;
            let regression_pct = (candidate_median / baseline_median - 1.0) * 100.0;
            Ok(RegressionComparison {
                case_key,
                baseline_median,
                candidate_median,
                regression_pct,
                threshold_pct,
                passed: regression_pct <= threshold_pct + 1.0e-12,
            })
        })
        .collect()
}

pub fn regression_gate_error(
    comparisons: &[RegressionComparison],
    gate_name: &str,
    unit: &str,
) -> Option<String> {
    let failures = comparisons
        .iter()
        .filter(|comparison| !comparison.passed)
        .map(|comparison| {
            format!(
                "{}: baseline {:.3} {}, candidate {:.3} {}, regression {:.3}% > threshold {:.3}%",
                comparison.case_key,
                comparison.baseline_median,
                unit,
                comparison.candidate_median,
                unit,
                comparison.regression_pct,
                comparison.threshold_pct
            )
        })
        .collect::<Vec<_>>();

    (!failures.is_empty()).then(|| format!("{gate_name}: {}", failures.join("; ")))
}

fn unique_case_map(
    cases: impl IntoIterator<Item = (String, f64)>,
    label: &str,
) -> Result<BTreeMap<String, f64>, String> {
    let mut indexed = BTreeMap::new();
    for (key, median) in cases {
        if indexed.insert(key.clone(), median).is_some() {
            return Err(format!(
                "{label} report contains duplicate case key '{key}'"
            ));
        }
    }
    if indexed.is_empty() {
        return Err(format!("{label} report contains no cases"));
    }
    Ok(indexed)
}

fn validate_median(case_key: &str, label: &str, median: f64) -> Result<(), String> {
    if median.is_finite() && median > 0.0 {
        Ok(())
    } else {
        Err(format!(
            "{label} case '{case_key}' median must be finite and positive, got {median}"
        ))
    }
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

pub fn read_json<T: for<'de> Deserialize<'de>>(
    path: &Path,
    report_name: &str,
) -> Result<T, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("failed to read '{}': {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| {
        format!(
            "failed to deserialize {report_name} from '{}': {error}",
            path.display()
        )
    })
}

pub fn environment_json(environment: &BenchEnvironment) -> Result<String, String> {
    serde_json::to_string(environment)
        .map_err(|error| format!("failed to serialize benchmark environment: {error}"))
}
