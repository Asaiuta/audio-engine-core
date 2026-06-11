thread_local! {
    static AUDIO_THREAD_FLOAT_MODE_INITIALIZED: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// Initialize floating-point mode for a real-time audio thread.
///
/// FTZ (flush-to-zero) and DAZ (denormals-are-zero, where available) are
/// thread-local CPU flags. Set them from the actual callback/playback thread so
/// biquad tails cannot fall into slow subnormal arithmetic.
pub fn audio_thread_init() {
    AUDIO_THREAD_FLOAT_MODE_INITIALIZED.with(|initialized| {
        if initialized.get() {
            return;
        }
        set_audio_thread_float_mode();
        initialized.set(true);
    });
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn set_audio_thread_float_mode() {
    const DAZ_BIT: u32 = 1 << 6;
    const FTZ_BIT: u32 = 1 << 15;

    // SAFETY: MXCSR is thread-local on x86/x86_64. This only enables FTZ/DAZ
    // for the current audio thread and does not access memory or cross threads.
    unsafe {
        let mut mxcsr = read_mxcsr();
        mxcsr |= DAZ_BIT | FTZ_BIT;
        write_mxcsr(mxcsr);
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
unsafe fn read_mxcsr() -> u32 {
    let mut mxcsr = 0u32;
    // SAFETY: `stmxcsr` stores the current thread's MXCSR into a valid local
    // stack slot. It does not dereference any caller-provided pointer.
    unsafe {
        std::arch::asm!("stmxcsr [{}]", in(reg) &mut mxcsr, options(nostack, preserves_flags));
    }
    mxcsr
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
unsafe fn write_mxcsr(mxcsr: u32) {
    // SAFETY: `ldmxcsr` loads the current thread's MXCSR from a valid local
    // stack slot. The caller supplies only FTZ/DAZ changes over the prior value.
    unsafe {
        std::arch::asm!("ldmxcsr [{}]", in(reg) &mxcsr, options(nostack, preserves_flags));
    }
}

#[cfg(target_arch = "aarch64")]
fn set_audio_thread_float_mode() {
    let mut fpcr: u64;
    // SAFETY: FPCR is a thread-local floating-point control register. We only
    // set bit 24 (FZ) for the current audio thread.
    unsafe {
        std::arch::asm!("mrs {fpcr}, fpcr", fpcr = out(reg) fpcr);
        fpcr |= 1 << 24;
        std::arch::asm!("msr fpcr, {fpcr}", fpcr = in(reg) fpcr);
    }
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
fn set_audio_thread_float_mode() {
    log::warn!("Audio thread FTZ/DAZ mode is unsupported on this CPU architecture");
}

#[cfg(any(test, debug_assertions))]
pub fn audio_thread_float_mode_is_enabled() -> bool {
    audio_thread_init();
    audio_thread_float_mode_is_enabled_unchecked()
}

#[cfg(all(
    any(test, debug_assertions),
    any(target_arch = "x86", target_arch = "x86_64")
))]
fn audio_thread_float_mode_is_enabled_unchecked() -> bool {
    const DAZ_BIT: u32 = 1 << 6;
    const FTZ_BIT: u32 = 1 << 15;

    // SAFETY: Reading MXCSR is thread-local and has no memory side effects.
    let csr = unsafe { read_mxcsr() };
    csr & (DAZ_BIT | FTZ_BIT) == (DAZ_BIT | FTZ_BIT)
}

#[cfg(all(any(test, debug_assertions), target_arch = "aarch64"))]
fn audio_thread_float_mode_is_enabled_unchecked() -> bool {
    let fpcr: u64;
    // SAFETY: Reading FPCR is thread-local and has no memory side effects.
    unsafe {
        std::arch::asm!("mrs {fpcr}, fpcr", fpcr = out(reg) fpcr);
    }
    fpcr & (1 << 24) != 0
}

#[cfg(all(
    any(test, debug_assertions),
    not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64"))
))]
fn audio_thread_float_mode_is_enabled_unchecked() -> bool {
    false
}

#[inline(always)]
pub fn flush_subnormal_sample(sample: f64) -> f64 {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64"))]
    {
        sample
    }

    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
    {
        if sample != 0.0 && sample.abs() < f64::MIN_POSITIVE {
            0.0
        } else {
            sample
        }
    }
}
