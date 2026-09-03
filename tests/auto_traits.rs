//! Compile-time guards for the stable auto-trait surface.

use audio_engine_core::{processor::FirEq, StreamingResampler};
use std::panic::{RefUnwindSafe, UnwindSafe};

fn assert_public_auto_traits<T: Send + Sync + UnwindSafe + RefUnwindSafe>() {}

#[test]
fn public_processors_retain_their_auto_traits() {
    assert_public_auto_traits::<FirEq>();
    assert_public_auto_traits::<StreamingResampler>();
}
