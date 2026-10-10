use super::{rust_backend::zeroed, BackendInitError as InitError};

/// A checked, naturally aligned subslice without a custom allocator or raw
/// ownership. SIMD coefficient rows retain the reference's 32-byte alignment.
pub(super) struct AlignedBuffer {
    storage: Vec<f64>,
    start: usize,
    length: usize,
}

impl AlignedBuffer {
    pub(super) fn zeroed(length: usize) -> Result<Self, InitError> {
        let padded = length.checked_add(3).ok_or(InitError::StorageOverflow {
            buffer: "aligned resampler coefficients",
        })?;
        let storage = zeroed(padded, 1, "aligned resampler coefficients")?;
        let start = storage.as_ptr().align_offset(32);
        // Some 32-bit allocators may align f64 only to four bytes. An f64
        // stride then cannot reach 32-byte alignment; unaligned SIMD loads
        // remain valid, so keep the original valid slice in that case.
        let start = if start <= 3 { start } else { 0 };
        Ok(Self {
            storage,
            start,
            length,
        })
    }
}

impl std::ops::Deref for AlignedBuffer {
    type Target = [f64];
    fn deref(&self) -> &[f64] {
        &self.storage[self.start..self.start + self.length]
    }
}

impl std::ops::DerefMut for AlignedBuffer {
    fn deref_mut(&mut self) -> &mut [f64] {
        &mut self.storage[self.start..self.start + self.length]
    }
}
