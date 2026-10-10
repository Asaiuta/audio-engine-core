//! Runtime-selected sinc arithmetic. Reduction trees follow Rubato 4.0 (MIT).
//! Unaligned loads support the mirrored history and phase-shifted filters.
type Function = unsafe fn(*const f64, *const f64, usize) -> f64;
type AccumulateFunction = unsafe fn(*const f64, *mut f64, f64, usize);
type PairFunction = unsafe fn(*const f64, *const f64, *const f64, usize, usize) -> [f64; 2];
type CombineFunction = unsafe fn(&[*const f64; 4], &[usize; 4], &[f64; 4], *mut f64, usize, usize);

#[derive(Clone, Copy)]
pub(super) struct Dot {
    function: Function,
    accumulate_function: AccumulateFunction,
    combine_function: Option<CombineFunction>,
    pair_function: Option<PairFunction>,
}

impl Dot {
    #[cfg(test)]
    pub(super) fn scalar_path() -> Self {
        Self {
            function: scalar,
            accumulate_function: accumulate_scalar,
            combine_function: None,
            pair_function: None,
        }
    }

    #[cfg(all(test, target_arch = "x86_64"))]
    pub(super) fn sse_path() -> Option<Self> {
        std::is_x86_feature_detected!("sse3").then_some(Self {
            function: sinc_sse,
            accumulate_function: accumulate_scalar,
            combine_function: None,
            pair_function: None,
        })
    }
    pub(super) fn sinc_compatible() -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            if std::is_x86_feature_detected!("avx") && std::is_x86_feature_detected!("fma") {
                return Self {
                    function: sinc_avx_fma,
                    accumulate_function: accumulate_avx_fma,
                    combine_function: Some(combine_avx_fma),
                    pair_function: Some(pair_avx_fma),
                };
            }
            if std::is_x86_feature_detected!("sse3") {
                return Self {
                    function: sinc_sse,
                    accumulate_function: accumulate_scalar,
                    combine_function: None,
                    pair_function: None,
                };
            }
        }
        #[cfg(target_arch = "aarch64")]
        if std::arch::is_aarch64_feature_detected!("neon") {
            return Self {
                function: sinc_neon,
                accumulate_function: accumulate_neon,
                combine_function: None,
                pair_function: None,
            };
        }
        Self {
            function: scalar,
            accumulate_function: accumulate_scalar,
            combine_function: None,
            pair_function: None,
        }
    }

    #[inline]
    pub(super) fn sample(self, signal: &[f64], filter: &[f64]) -> f64 {
        let n = signal.len().min(filter.len());
        // SAFETY: slices supply n readable elements; setup checked CPU support.
        unsafe { (self.function)(signal.as_ptr(), filter.as_ptr(), n) }
    }

    #[inline]
    pub(super) fn sample_pair(
        self,
        signal: &[f64],
        first: &[f64],
        second: &[f64],
        shift: usize,
    ) -> [f64; 2] {
        let n = first
            .len()
            .min(second.len())
            .min(signal.len().saturating_sub(shift));
        if n == 0 {
            return [0.0; 2];
        }
        if let Some(function) = self.pair_function {
            // SAFETY: both filters supply n elements and signal supplies
            // n + shift; setup checked the selected instruction set.
            unsafe { function(signal.as_ptr(), first.as_ptr(), second.as_ptr(), n, shift) }
        } else {
            [
                self.sample(&signal[..n], &first[..n]),
                self.sample(&signal[shift..shift + n], &second[..n]),
            ]
        }
    }
    pub(super) fn accumulate(self, source: &[f64], output: &mut [f64], scale: f64) {
        let n = source.len().min(output.len());
        // SAFETY: slices supply n disjoint readable/writable elements and setup
        // checked every instruction set used by the selected function.
        unsafe { (self.accumulate_function)(source.as_ptr(), output.as_mut_ptr(), scale, n) }
    }

    #[inline]
    pub(super) fn combine(
        self,
        filters: [&[f64]; 4],
        shifts: [usize; 4],
        weights: [f64; 4],
        points: usize,
        output: &mut [f64],
    ) {
        let points = points.min(4);
        if points == 0 {
            output.fill(0.0);
            return;
        }
        let max_shift = shifts[..points].iter().copied().max().unwrap_or(0);
        let n = filters[..points]
            .iter()
            .map(|row| row.len())
            .min()
            .unwrap_or(0)
            .min(output.len().saturating_sub(max_shift));
        if n == 0 {
            output.fill(0.0);
            return;
        }
        if let Some(function) = self.combine_function {
            if max_shift <= 1 && output.len() > n && (points == 2 || points == 4) {
                let pointers = filters.map(|row| row.as_ptr());
                // SAFETY: every used filter supplies n elements, shifts are
                // 0/1, output supplies n+1 elements, and setup checked AVX/FMA.
                unsafe {
                    function(&pointers, &shifts, &weights, output.as_mut_ptr(), n, points);
                }
                output[n + 1..].fill(0.0);
                return;
            }
        }
        output.fill(0.0);
        for j in 0..points {
            self.accumulate(
                &filters[j][..n],
                &mut output[shifts[j]..shifts[j] + n],
                weights[j],
            );
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx,fma")]
unsafe fn pair_avx_fma(
    signal: *const f64,
    first: *const f64,
    second: *const f64,
    n: usize,
    shift: usize,
) -> [f64; 2] {
    // SAFETY: the safe entry validated both complete signal windows and
    // filters. The const choice removes the shared-window branch from the loop.
    unsafe {
        if shift == 0 {
            pair_avx_body::<true>(signal, first, second, n, shift)
        } else {
            pair_avx_body::<false>(signal, first, second, n, shift)
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[inline]
#[target_feature(enable = "avx,fma")]
unsafe fn pair_avx_body<const SHARED: bool>(
    signal: *const f64,
    first: *const f64,
    second: *const f64,
    n: usize,
    shift: usize,
) -> [f64; 2] {
    use std::arch::x86_64::*;
    let mut a = [_mm256_setzero_pd(); 4];
    let mut b = [_mm256_setzero_pd(); 4];
    // Preserve four independent accumulators and the original reduction
    // tree for each result; only independent filter evaluations interleave.
    unsafe {
        for group in 0..n / 16 {
            for j in 0..4 {
                let i = group * 16 + j * 4;
                let x = _mm256_loadu_pd(signal.add(i));
                let y = if SHARED {
                    x
                } else {
                    _mm256_loadu_pd(signal.add(i + shift))
                };
                a[j] = _mm256_fmadd_pd(x, _mm256_loadu_pd(first.add(i)), a[j]);
                b[j] = _mm256_fmadd_pd(y, _mm256_loadu_pd(second.add(i)), b[j]);
            }
        }
        let mut i = (n / 16) * 16;
        while i + 4 <= n {
            let x = _mm256_loadu_pd(signal.add(i));
            let y = if SHARED {
                x
            } else {
                _mm256_loadu_pd(signal.add(i + shift))
            };
            a[0] = _mm256_fmadd_pd(x, _mm256_loadu_pd(first.add(i)), a[0]);
            b[0] = _mm256_fmadd_pd(y, _mm256_loadu_pd(second.add(i)), b[0]);
            i += 4;
        }
        let mut result = [0.0; 2];
        for (out, acc) in result.iter_mut().zip([a, b]) {
            let sum = _mm256_add_pd(_mm256_add_pd(acc[0], acc[1]), _mm256_add_pd(acc[2], acc[3]));
            let halves = _mm_add_pd(_mm256_extractf128_pd(sum, 1), _mm256_castpd256_pd128(sum));
            *out = _mm_cvtsd_f64(_mm_hadd_pd(halves, halves));
        }
        while i < n {
            result[0] += *signal.add(i) * *first.add(i);
            result[1] += *signal.add(i + shift) * *second.add(i);
            i += 1;
        }
        result
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx,fma")]
unsafe fn combine_avx_fma(
    filters: &[*const f64; 4],
    shifts: &[usize; 4],
    weights: &[f64; 4],
    output: *mut f64,
    n: usize,
    points: usize,
) {
    use std::arch::x86_64::*;
    // Each coefficient retains the original phase-order FMA chain. Combining
    // independent coefficients in one pass removes repeated read/modify/write
    // passes over the destination without changing any reduction order.
    unsafe {
        let scales = weights.map(|weight| _mm256_set1_pd(weight));
        for edge in [0, n] {
            let mut value = 0.0;
            for j in 0..points {
                if edge >= shifts[j] && edge - shifts[j] < n {
                    value = weights[j].mul_add(*filters[j].add(edge - shifts[j]), value);
                }
            }
            *output.add(edge) = value;
        }
        let mut i = 1;
        while i + 4 <= n {
            let mut value = _mm256_fmadd_pd(
                scales[0],
                _mm256_loadu_pd(filters[0].add(i - shifts[0])),
                _mm256_setzero_pd(),
            );
            value = _mm256_fmadd_pd(
                scales[1],
                _mm256_loadu_pd(filters[1].add(i - shifts[1])),
                value,
            );
            if points == 4 {
                value = _mm256_fmadd_pd(
                    scales[2],
                    _mm256_loadu_pd(filters[2].add(i - shifts[2])),
                    value,
                );
                value = _mm256_fmadd_pd(
                    scales[3],
                    _mm256_loadu_pd(filters[3].add(i - shifts[3])),
                    value,
                );
            }
            _mm256_storeu_pd(output.add(i), value);
            i += 4;
        }
        while i < n {
            let mut value = 0.0;
            for j in 0..points {
                value = weights[j].mul_add(*filters[j].add(i - shifts[j]), value);
            }
            *output.add(i) = value;
            i += 1;
        }
    }
}

unsafe fn accumulate_scalar(source: *const f64, output: *mut f64, scale: f64, n: usize) {
    // SAFETY: caller guarantees n readable/writable disjoint elements.
    unsafe {
        for i in 0..n {
            *output.add(i) += scale * *source.add(i);
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx,fma")]
unsafe fn accumulate_avx_fma(source: *const f64, output: *mut f64, scale: f64, n: usize) {
    use std::arch::x86_64::*;
    let mut i = 0;
    let scale_vec = _mm256_set1_pd(scale);
    // SAFETY: every vector and scalar access stays within the supplied slices.
    unsafe {
        while i + 4 <= n {
            _mm256_storeu_pd(
                output.add(i),
                _mm256_fmadd_pd(
                    scale_vec,
                    _mm256_loadu_pd(source.add(i)),
                    _mm256_loadu_pd(output.add(i)),
                ),
            );
            i += 4;
        }
        while i < n {
            *output.add(i) = scale.mul_add(*source.add(i), *output.add(i));
            i += 1;
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx,fma")]
unsafe fn sinc_avx_fma(x: *const f64, h: *const f64, n: usize) -> f64 {
    use std::arch::x86_64::*;
    let mut a = _mm256_setzero_pd();
    let mut b = _mm256_setzero_pd();
    let mut c = _mm256_setzero_pd();
    let mut d = _mm256_setzero_pd();
    // SAFETY: each full vector has four valid elements; the suffix is scalar.
    unsafe {
        for group in 0..n / 16 {
            let i = group * 16;
            a = _mm256_fmadd_pd(_mm256_loadu_pd(x.add(i)), _mm256_loadu_pd(h.add(i)), a);
            b = _mm256_fmadd_pd(
                _mm256_loadu_pd(x.add(i + 4)),
                _mm256_loadu_pd(h.add(i + 4)),
                b,
            );
            c = _mm256_fmadd_pd(
                _mm256_loadu_pd(x.add(i + 8)),
                _mm256_loadu_pd(h.add(i + 8)),
                c,
            );
            d = _mm256_fmadd_pd(
                _mm256_loadu_pd(x.add(i + 12)),
                _mm256_loadu_pd(h.add(i + 12)),
                d,
            );
        }
        let mut i = (n / 16) * 16;
        while i + 4 <= n {
            a = _mm256_fmadd_pd(_mm256_loadu_pd(x.add(i)), _mm256_loadu_pd(h.add(i)), a);
            i += 4;
        }
        let sum = _mm256_add_pd(_mm256_add_pd(a, b), _mm256_add_pd(c, d));
        let halves = _mm_add_pd(_mm256_extractf128_pd(sum, 1), _mm256_castpd256_pd128(sum));
        let mut value = _mm_cvtsd_f64(_mm_hadd_pd(halves, halves));
        while i < n {
            value += *x.add(i) * *h.add(i);
            i += 1;
        }
        value
    }
}

unsafe fn scalar(x: *const f64, h: *const f64, n: usize) -> f64 {
    let mut a = [0.0; 8];
    let mut i = 0;
    // SAFETY: vector-sized groups and suffix both stay within n elements.
    unsafe {
        while i + 8 <= n {
            for (j, value) in a.iter_mut().enumerate() {
                *value += *x.add(i + j) * *h.add(i + j);
            }
            i += 8;
        }
        let mut value = a[0] + a[1] + a[2] + a[3] + a[4] + a[5] + a[6] + a[7];
        while i < n {
            value += *x.add(i) * *h.add(i);
            i += 1;
        }
        value
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse3")]
unsafe fn sinc_sse(x: *const f64, h: *const f64, n: usize) -> f64 {
    use std::arch::x86_64::*;
    let mut a = [_mm_setzero_pd(); 8];
    let mut i = 0;
    // SAFETY: complete two-element vectors and scalar suffix are in bounds.
    unsafe {
        while i + 16 <= n {
            for (j, value) in a.iter_mut().enumerate() {
                *value = _mm_add_pd(
                    *value,
                    _mm_mul_pd(
                        _mm_loadu_pd(x.add(i + 2 * j)),
                        _mm_loadu_pd(h.add(i + 2 * j)),
                    ),
                );
            }
            i += 16;
        }
        while i + 2 <= n {
            a[0] = _mm_add_pd(
                a[0],
                _mm_mul_pd(_mm_loadu_pd(x.add(i)), _mm_loadu_pd(h.add(i))),
            );
            i += 2;
        }
        let left = _mm_add_pd(_mm_add_pd(a[0], a[1]), _mm_add_pd(a[2], a[3]));
        let right = _mm_add_pd(_mm_add_pd(a[4], a[5]), _mm_add_pd(a[6], a[7]));
        let packed = _mm_hadd_pd(left, right);
        let mut value = _mm_cvtsd_f64(_mm_hadd_pd(packed, packed));
        while i < n {
            value += *x.add(i) * *h.add(i);
            i += 1;
        }
        value
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn sinc_neon(x: *const f64, h: *const f64, n: usize) -> f64 {
    use std::arch::aarch64::*;
    let mut a = [vmovq_n_f64(0.0); 8];
    let mut i = 0;
    // SAFETY: complete two-element vectors and scalar suffix are in bounds.
    unsafe {
        while i + 16 <= n {
            for (j, value) in a.iter_mut().enumerate() {
                *value = vfmaq_f64(
                    *value,
                    vld1q_f64(x.add(i + 2 * j)),
                    vld1q_f64(h.add(i + 2 * j)),
                );
            }
            i += 16;
        }
        while i + 2 <= n {
            a[0] = vfmaq_f64(a[0], vld1q_f64(x.add(i)), vld1q_f64(h.add(i)));
            i += 2;
        }
        let sum = vaddq_f64(
            vaddq_f64(vaddq_f64(a[0], a[1]), vaddq_f64(a[2], a[3])),
            vaddq_f64(vaddq_f64(a[4], a[5]), vaddq_f64(a[6], a[7])),
        );
        let mut value = vgetq_lane_f64::<0>(sum) + vgetq_lane_f64::<1>(sum);
        while i < n {
            value += *x.add(i) * *h.add(i);
            i += 1;
        }
        value
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn accumulate_neon(source: *const f64, output: *mut f64, scale: f64, n: usize) {
    use std::arch::aarch64::*;
    let mut i = 0;
    let scale_vec = vmovq_n_f64(scale);
    // SAFETY: complete vectors and scalar suffix are within disjoint slices.
    unsafe {
        while i + 2 <= n {
            vst1q_f64(
                output.add(i),
                vfmaq_f64(
                    vld1q_f64(output.add(i)),
                    scale_vec,
                    vld1q_f64(source.add(i)),
                ),
            );
            i += 2;
        }
        while i < n {
            *output.add(i) = scale.mul_add(*source.add(i), *output.add(i));
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paired_filters_match_independent_dots_bit_for_bit() {
        let mut paths = vec![Dot::sinc_compatible(), Dot::scalar_path()];
        #[cfg(target_arch = "x86_64")]
        if let Some(sse) = Dot::sse_path() {
            paths.push(sse);
        }
        for n in 0..=257 {
            for shift in [0, 1, 3] {
                let x: Vec<_> = (0..n + shift + 3)
                    .map(|i| (i as f64 * 0.719).sin())
                    .collect();
                let first: Vec<_> = (0..n + 1)
                    .map(|i| (i as f64 * 0.317).cos() / 257.0)
                    .collect();
                let second: Vec<_> = (0..n + 2)
                    .map(|i| (i as f64 * 0.511).sin() / 257.0)
                    .collect();
                for path in &paths {
                    let expected = [
                        path.sample(&x[3..3 + n], &first[1..]),
                        path.sample(&x[3 + shift..], &second[2..]),
                    ];
                    let actual = assert_no_alloc::assert_no_alloc(|| {
                        path.sample_pair(&x[3..], &first[1..], &second[2..], shift)
                    });
                    assert_eq!(
                        actual.map(f64::to_bits),
                        expected.map(f64::to_bits),
                        "{n}/{shift}"
                    );
                }
            }
        }
    }

    #[test]
    fn fused_filter_combination_preserves_sequential_phase_arithmetic() {
        let mut paths = vec![Dot::sinc_compatible(), Dot::scalar_path()];
        #[cfg(target_arch = "x86_64")]
        if let Some(sse) = Dot::sse_path() {
            paths.push(sse);
        }
        for n in [0, 1, 2, 3, 4, 5, 17, 63, 64, 128, 256, 257] {
            let storage: [Vec<f64>; 4] = std::array::from_fn(|j| {
                (0..n + j + 1)
                    .map(|i| ((i * 31 + j * 7) as f64 * 0.319).sin())
                    .collect()
            });
            let filters = std::array::from_fn(|j| &storage[j][j + 1..]);
            for points in [0_usize, 1, 2, 3, 4, 5] {
                for pattern in 0..17 {
                    let shifts = if pattern == 16 {
                        [0, 2, 1, 3]
                    } else {
                        std::array::from_fn(|j| (pattern >> j) & 1)
                    };
                    for weights in [[0.137, -0.713, 0.319, -1.231], [0.0, -0.0, 0.5, -0.5]] {
                        for extra in [0, 1, 3] {
                            let size = n + extra;
                            let used = points.min(4);
                            let shift = shifts[..used].iter().copied().max().unwrap_or(0);
                            let count = n.min(size.saturating_sub(shift));
                            for path in &paths {
                                let mut expected = vec![0.0; size];
                                if count != 0 {
                                    for j in 0..used {
                                        path.accumulate(
                                            &filters[j][..count],
                                            &mut expected[shifts[j]..shifts[j] + count],
                                            weights[j],
                                        );
                                    }
                                }
                                let mut actual = vec![13.0; size + 2];
                                assert_no_alloc::assert_no_alloc(|| {
                                    path.combine(
                                        filters,
                                        shifts,
                                        weights,
                                        points,
                                        &mut actual[1..size + 1],
                                    );
                                });
                                assert_eq!(actual[0], 13.0);
                                assert_eq!(actual[size + 1], 13.0);
                                for (a, b) in actual[1..size + 1].iter().zip(&expected) {
                                    assert_eq!(
                                        a.to_bits(),
                                        b.to_bits(),
                                        "n={n}, points={points}, shifts={shifts:?}, extra={extra}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn every_available_path_handles_unaligned_inputs_and_suffixes() {
        let mut paths = vec![Dot::sinc_compatible(), Dot::scalar_path()];
        #[cfg(target_arch = "x86_64")]
        if let Some(sse) = Dot::sse_path() {
            paths.push(sse);
        }
        for n in 0..=257 {
            let x: Vec<_> = (0..n + 3).map(|i| (i as f64 * 0.713).sin()).collect();
            let h: Vec<_> = (0..n + 5)
                .map(|i| (i as f64 * 0.311).cos() / 257.0)
                .collect();
            let oracle: f64 = x[1..n + 1]
                .iter()
                .zip(&h[3..n + 3])
                .map(|(a, b)| a * b)
                .sum();
            for path in &paths {
                let actual = path.sample(&x[1..n + 1], &h[3..n + 3]);
                assert!((actual - oracle).abs() < 3e-15, "{n}");
                let mut output = vec![0.13; n + 2];
                assert_no_alloc::assert_no_alloc(|| {
                    path.accumulate(&x[1..n + 1], &mut output[1..n + 1], -0.371)
                });
                assert_eq!(output[0], 0.13);
                assert_eq!(output[n + 1], 0.13);
                for i in 0..n {
                    assert!((output[i + 1] - (0.13 - 0.371 * x[i + 1])).abs() < 2e-16);
                }
            }
        }
    }
}
