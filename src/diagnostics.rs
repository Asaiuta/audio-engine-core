use serde::Serialize;

const BYTES_PER_MIB_USIZE: usize = 1024 * 1024;

/// Decoded-buffer budget shared by non-Range downloads, playback loads, and
/// gapless preload.
/// Environment variable name for the decode-buffer memory budget override.
pub const ENV_DECODE_MAX_MEMORY_MB: &str = "DECODE_MAX_MEMORY_MB";
/// Built-in decode-buffer budget (mebibytes) when the environment is absent.
pub const DEFAULT_DECODE_MAX_MEMORY_MB: usize = 2048;
/// Smallest accepted decode-buffer budget (mebibytes) before clamping.
pub const MIN_DECODE_MAX_MEMORY_MB: usize = 64;
/// Largest accepted decode-buffer budget (mebibytes) before clamping.
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
/// [`DEFAULT_DECODE_MAX_MEMORY_MB`] and clamps to the configured bounds plus
/// the current target's `isize::MAX` single-allocation ceiling.
pub fn decode_memory_budget() -> DecodeMemoryBudget {
    let configured = std::env::var(ENV_DECODE_MAX_MEMORY_MB)
        .ok()
        .and_then(|value| value.parse::<usize>().ok());

    resolve_decode_memory_budget(configured, isize::MAX as usize)
}

fn resolve_decode_memory_budget(
    configured: Option<usize>,
    max_single_allocation_bytes: usize,
) -> DecodeMemoryBudget {
    let target_max_mb = max_single_allocation_bytes / BYTES_PER_MIB_USIZE;
    let effective_max_mb = MAX_DECODE_MAX_MEMORY_MB.min(target_max_mb);
    let effective_min_mb = MIN_DECODE_MAX_MEMORY_MB.min(effective_max_mb);
    let limit_mb = configured
        .unwrap_or(DEFAULT_DECODE_MAX_MEMORY_MB)
        .clamp(effective_min_mb, effective_max_mb);
    let limit_bytes = limit_mb
        .saturating_mul(BYTES_PER_MIB_USIZE)
        .min(max_single_allocation_bytes);

    DecodeMemoryBudget {
        limit_mb,
        limit_bytes,
        source: if configured.is_some() {
            ENV_DECODE_MAX_MEMORY_MB
        } else {
            "default"
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        resolve_decode_memory_budget, DEFAULT_DECODE_MAX_MEMORY_MB, ENV_DECODE_MAX_MEMORY_MB,
        MAX_DECODE_MAX_MEMORY_MB, MIN_DECODE_MAX_MEMORY_MB,
    };

    #[test]
    fn simulated_32_bit_budget_respects_vec_allocation_ceiling() {
        let default_budget = resolve_decode_memory_budget(None, i32::MAX as usize);
        assert_eq!(default_budget.limit_mb, 2_047);
        assert_eq!(default_budget.limit_bytes, 2_047 * 1024 * 1024);
        assert_eq!(default_budget.source, "default");

        let maximum_override =
            resolve_decode_memory_budget(Some(MAX_DECODE_MAX_MEMORY_MB), i32::MAX as usize);
        assert_eq!(maximum_override.limit_mb, 2_047);
        assert_eq!(maximum_override.limit_bytes, 2_047 * 1024 * 1024);
        assert_eq!(maximum_override.source, ENV_DECODE_MAX_MEMORY_MB);
    }

    #[test]
    fn native_budget_preserves_configured_clamps_and_sources() {
        let native_max_mb = (isize::MAX as usize) / (1024 * 1024);
        let default_budget = resolve_decode_memory_budget(None, isize::MAX as usize);
        assert_eq!(
            default_budget.limit_mb,
            DEFAULT_DECODE_MAX_MEMORY_MB.min(native_max_mb)
        );
        assert_eq!(default_budget.source, "default");

        let minimum_override = resolve_decode_memory_budget(Some(1), isize::MAX as usize);
        assert_eq!(
            minimum_override.limit_mb,
            MIN_DECODE_MAX_MEMORY_MB.min(native_max_mb)
        );
        assert_eq!(minimum_override.source, ENV_DECODE_MAX_MEMORY_MB);
    }
}
