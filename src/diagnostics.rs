use serde::Serialize;

const BYTES_PER_MIB_USIZE: usize = 1024 * 1024;

/// Decoded-buffer budget shared by non-Range downloads, playback loads, and
/// gapless preload.
pub const ENV_DECODE_MAX_MEMORY_MB: &str = "DECODE_MAX_MEMORY_MB";
pub const DEFAULT_DECODE_MAX_MEMORY_MB: usize = 2048;
pub const MIN_DECODE_MAX_MEMORY_MB: usize = 64;
pub const MAX_DECODE_MAX_MEMORY_MB: usize = 32 * 1024;

/// Resolved decode-buffer memory budget.
///
/// Returned by [`decode_memory_budget`]; reports the effective limit and
/// whether it came from the environment override or the built-in default.
#[derive(Debug, Clone, Serialize)]
pub struct DecodeMemoryBudget {
    /// Effective limit in mebibytes.
    pub limit_mb: usize,
    /// Effective limit in bytes (`limit_mb * 1024 * 1024`).
    pub limit_bytes: usize,
    /// Origin of the limit: the env var name when overridden, else `"default"`.
    pub source: &'static str,
}

/// Resolve the decode-buffer memory budget from the environment.
///
/// Reads [`ENV_DECODE_MAX_MEMORY_MB`]; falls back to
/// [`DEFAULT_DECODE_MAX_MEMORY_MB`] and clamps to
/// `[MIN_DECODE_MAX_MEMORY_MB, MAX_DECODE_MAX_MEMORY_MB]`.
pub fn decode_memory_budget() -> DecodeMemoryBudget {
    let configured = std::env::var(ENV_DECODE_MAX_MEMORY_MB)
        .ok()
        .and_then(|value| value.parse::<usize>().ok());
    let limit_mb = configured
        .unwrap_or(DEFAULT_DECODE_MAX_MEMORY_MB)
        .clamp(MIN_DECODE_MAX_MEMORY_MB, MAX_DECODE_MAX_MEMORY_MB);

    DecodeMemoryBudget {
        limit_mb,
        limit_bytes: limit_mb * BYTES_PER_MIB_USIZE,
        source: if configured.is_some() {
            ENV_DECODE_MAX_MEMORY_MB
        } else {
            "default"
        },
    }
}
