//! Decode-buffer memory budget compatibility facade.
//!
//! The implementation moved to [`crate::decode_budget`]; this module retains
//! the historical public path so existing `audio_engine_core::diagnostics::*`
//! imports keep resolving to the same items. New code should import from
//! `crate::decode_budget`.

pub use crate::decode_budget::{
    decode_memory_budget, DecodeMemoryBudget, DEFAULT_DECODE_MAX_MEMORY_MB,
    ENV_DECODE_MAX_MEMORY_MB, MAX_DECODE_MAX_MEMORY_MB, MIN_DECODE_MAX_MEMORY_MB,
};
