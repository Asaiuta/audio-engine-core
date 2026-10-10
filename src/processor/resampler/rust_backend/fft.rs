//! Synchronous FFT control. See NOTICE: filter constants are from Rubato.

use super::{gcd, samples, zeroed, Config, InitError, ProcessError, Quality};
use realfft::{RealToComplex, RealToComplexEven};
use rustfft::num_complex::Complex64 as Complex;
use rustfft::{Fft, FftPlanner};
use std::{f64::consts::PI, sync::Arc};
use windowfunctions::{window, Symmetry, WindowFunction};

#[derive(Clone, Copy, Debug)]
pub(super) struct Geometry {
    pub input: usize,
    pub output: usize,
}
impl Geometry {
    pub(super) fn reference(config: Config) -> Result<Self, InitError> {
        config.validate()?;
        let divisor = gcd(config.from, config.to);
        let down = (config.from / divisor) as usize;
        let up = (config.to / divisor) as usize;
        if down > 1024 || up > 1024 {
            return Err(InitError::InvalidGeometry {
                backend: "owned FFT",
            });
        }
        let hint: usize = if config.quality == Quality::UltraHigh {
            1024
        } else {
            512
        };
        let units = hint.div_ceil(down);
        Ok(Self {
            input: units * down,
            output: units * up,
        })
    }
}

pub(super) fn reference_filter(geometry: Geometry) -> Vec<f64> {
    let n = geometry.input.min(geometry.output) as f32;
    let k1 = 13.745_202_940_783_823_f64 as f32;
    let k2 = 121.735_325_863_749_34_f64 as f32;
    let k3 = 5_964.163_279_612_051_f64 as f32;
    let mut cutoff = 1.0 / (k1 / n + k2 / (n * n) + k3 / (n * n * n) + 1.0);
    if geometry.input > geometry.output {
        cutoff = cutoff * geometry.output as f32 / geometry.input as f32;
    }
    let mut taps = Vec::with_capacity(geometry.input);
    let mut sum = 0.0;
    for (i, w) in window::<f64>(
        geometry.input,
        WindowFunction::BlackmanHarris,
        Symmetry::Periodic,
    )
    .enumerate()
    {
        let x = (i as f64 - (geometry.input / 2) as f64) * f64::from(cutoff);
        let sinc = if x == 0.0 {
            1.0
        } else {
            (PI * x).sin() / (PI * x)
        };
        let tap = (w * w) * sinc;
        taps.push(tap);
        sum += tap;
    }
    for tap in &mut taps {
        *tap /= sum;
    }
    taps
}

pub(super) struct FftKernel {
    pub geometry: Geometry,
    channels: usize,
    forward: std::panic::AssertUnwindSafe<Arc<dyn RealToComplex<f64>>>,
    inverse: std::panic::AssertUnwindSafe<Arc<dyn Fft<f64>>>,
    inverse_twiddles: Vec<Complex>,
    input_time: Vec<f64>,
    input_spectrum: Vec<Complex>,
    input_scratch: Vec<Complex>,
    output_time: Vec<f64>,
    output_spectrum: Vec<Complex>,
    output_scratch: Vec<Complex>,
    filter: Vec<Complex>,
    overlap: Vec<f64>,
}

impl FftKernel {
    pub(super) fn new(config: Config, geometry: Geometry, taps: &[f64]) -> Result<Self, InitError> {
        config.validate()?;
        if geometry.input == 0
            || geometry.output == 0
            || geometry.input > 1 << 20
            || geometry.output > 1 << 20
            || taps.len() > geometry.input + 1
            || taps.is_empty()
            || taps.iter().any(|t| !t.is_finite())
        {
            return Err(InitError::InvalidGeometry {
                backend: "owned FFT",
            });
        }
        samples(geometry.input, config.channels, "FFT input")?;
        let overlap = zeroed(geometry.output, config.channels, "FFT overlap")?;
        let mut planner = FftPlanner::<f64>::new();
        let forward: Arc<dyn RealToComplex<f64>> =
            Arc::new(RealToComplexEven::new(2 * geometry.input, &mut planner));
        let inverse = planner.plan_fft_inverse(geometry.output);
        let angle = -2.0 * PI / (2 * geometry.output) as f64;
        let inverse_twiddles = (0..geometry.output.div_ceil(2))
            .map(|k| {
                let phase = angle * k as f64;
                Complex::new(phase.cos(), -phase.sin())
            })
            .collect();
        let mut input_time = forward.make_input_vec();
        for (y, x) in input_time.iter_mut().zip(taps) {
            *y = *x / (2 * geometry.input) as f64;
        }
        let mut filter = forward.make_output_vec();
        let mut input_scratch = forward.make_scratch_vec();
        forward
            .process_with_scratch(&mut input_time, &mut filter, &mut input_scratch)
            .map_err(|error| InitError::Backend {
                message: error.to_string(),
            })?;
        Ok(Self {
            geometry,
            channels: config.channels,
            input_time,
            input_spectrum: forward.make_output_vec(),
            input_scratch,
            output_time: vec![0.0; 2 * geometry.output],
            output_spectrum: vec![Complex::default(); geometry.output],
            output_scratch: vec![Complex::default(); inverse.get_outofplace_scratch_len()],
            inverse_twiddles,
            filter,
            overlap,
            forward: std::panic::AssertUnwindSafe(forward),
            inverse: std::panic::AssertUnwindSafe(inverse),
        })
    }
    pub(super) fn reset(&mut self) {
        self.overlap.fill(0.0);
    }
    pub(super) fn block(&mut self, input: &[f64], output: &mut [f64]) -> Result<(), ProcessError> {
        let ni = self.geometry.input;
        let no = self.geometry.output;
        let channels = self.channels;
        if input.len() != ni * channels || output.len() != no * channels {
            return Err(ProcessError::new("incorrect FFT block dimensions"));
        }
        for ch in 0..channels {
            if channels == 1 {
                self.input_time[..ni].copy_from_slice(input);
            } else {
                for (i, x) in self.input_time[..ni].iter_mut().enumerate() {
                    *x = input[i * channels + ch];
                }
            }
            self.input_time[ni..].fill(0.0);
            self.forward
                .process_with_scratch(
                    &mut self.input_time,
                    &mut self.input_spectrum,
                    &mut self.input_scratch,
                )
                .map_err(|_| ProcessError::new("owned FFT transform failed"))?;
            self.prepare_inverse();
            // SAFETY: Complex<f64> is repr(C) with two f64 fields, the same
            // alignment as f64. The initialized 2*no scalar allocation is
            // exclusively borrowed as exactly no complex values for this call.
            let packed_time = unsafe {
                std::slice::from_raw_parts_mut(self.output_time.as_mut_ptr().cast::<Complex>(), no)
            };
            self.inverse.process_outofplace_with_scratch(
                &mut self.output_spectrum,
                packed_time,
                &mut self.output_scratch,
            );
            let overlap = &mut self.overlap[ch * no..(ch + 1) * no];
            for (i, old) in overlap.iter().enumerate() {
                output[i * channels + ch] = self.output_time[i] + old;
            }
            overlap.copy_from_slice(&self.output_time[no..]);
        }
        Ok(())
    }

    /// Fuse filtering with the real inverse's conjugate-pair preparation.
    ///
    /// The inverse still performs the same half-length complex FFT. Fusing
    /// these passes removes the intermediate filtered real spectrum and avoids
    /// processing known-zero high bins. Coefficients and normalization stay
    /// unchanged. The packing equations are adapted from RealFFT 3.5 (NOTICE).
    fn prepare_inverse(&mut self) {
        let ni = self.geometry.input;
        let no = self.geometry.output;
        let keep = if ni < no { ni + 1 } else { no };
        let pair_end = no.div_ceil(2);
        let active_end = keep.min(pair_end);
        let both_start = (no + 1 - keep).clamp(1, active_end);
        let dc = (self.input_spectrum[0] * self.filter[0]).re;
        // Output Nyquist is zero in both the existing up/downsampling maps.
        self.output_spectrum[0] = Complex::new(dc, dc);

        // Split disjoint output halves once. Contiguous zipped slices let LLVM
        // vectorize the arithmetic without per-bin indexed bounds checks.
        let (output_left, output_right) = self.output_spectrum.split_at_mut(pair_end);
        let right_single_start = no - both_start + 1 - pair_end;
        // Only the low member survives in this range of an upsampled spectrum.
        for (((out, out_rev), (input, filter)), twiddle) in output_left[1..both_start]
            .iter_mut()
            .zip(output_right[right_single_start..].iter_mut().rev())
            .zip(
                self.input_spectrum[1..both_start]
                    .iter()
                    .zip(&self.filter[1..both_start]),
            )
            .zip(&self.inverse_twiddles[1..both_start])
        {
            let value = *input * *filter;
            let real = value.im * twiddle.re + value.re * twiddle.im;
            let imag = value.im * twiddle.im - value.re * twiddle.re;
            *out = Complex::new(value.re - real, value.im - imag);
            *out_rev = Complex::new(value.re + real, -imag - value.im);
        }
        if both_start < active_end {
            let reverse_start = no - active_end + 1;
            let reverse_end = no - both_start + 1;
            for ((((out, out_rev), (input, filter)), (input_rev, filter_rev)), twiddle) in
                output_left[both_start..active_end]
                    .iter_mut()
                    .zip(
                        output_right[reverse_start - pair_end..reverse_end - pair_end]
                            .iter_mut()
                            .rev(),
                    )
                    .zip(
                        self.input_spectrum[both_start..active_end]
                            .iter()
                            .zip(&self.filter[both_start..active_end]),
                    )
                    .zip(
                        self.input_spectrum[reverse_start..reverse_end]
                            .iter()
                            .rev()
                            .zip(self.filter[reverse_start..reverse_end].iter().rev()),
                    )
                    .zip(&self.inverse_twiddles[both_start..active_end])
            {
                let left = *input * *filter;
                let right = *input_rev * *filter_rev;
                let sum = left + right;
                let diff = left - right;
                let real = sum.im * twiddle.re + diff.re * twiddle.im;
                let imag = sum.im * twiddle.im - diff.re * twiddle.re;
                *out = Complex::new(sum.re - real, diff.im - imag);
                *out_rev = Complex::new(sum.re + real, -imag - diff.im);
            }
        }
        let zero_end = no - active_end + 1;
        if active_end < zero_end {
            self.output_spectrum[active_end..zero_end].fill(Complex::default());
        }
        if no.is_multiple_of(2) && no / 2 < keep {
            let value = self.input_spectrum[no / 2] * self.filter[no / 2];
            self.output_spectrum[no / 2] = (value + value).conj();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use realfft::RealFftPlanner;

    #[test]
    fn fused_inverse_matches_independent_realfft_filtering() {
        // Cover odd/even packed transforms, up/down maps, one surviving bin,
        // and the production High/UltraHigh common-rate geometries.
        let geometries = [
            (1, 1),
            (1, 7),
            (7, 1),
            (3, 5),
            (5, 3),
            (4, 7),
            (7, 4),
            (588, 640),
            (640, 588),
            (588, 1280),
            (640, 294),
            (1176, 1280),
            (1280, 1176),
            (1176, 2560),
            (1280, 588),
        ];
        let mut maximum_error = 0.0f64;
        let mut compared = 0usize;
        for (ni, no) in geometries {
            let config = Config {
                from: ni as u32,
                to: no as u32,
                channels: 1,
                quality: Quality::High,
            };
            let geometry = Geometry {
                input: ni,
                output: no,
            };
            let mut kernel = FftKernel::new(config, geometry, &reference_filter(geometry)).unwrap();
            let mut planner = RealFftPlanner::<f64>::new();
            let inverse = planner.plan_fft_inverse(2 * no);
            let mut spectrum = inverse.make_input_vec();
            let mut expected = inverse.make_output_vec();
            let mut scratch = inverse.make_scratch_vec();
            let keep = if ni < no { ni + 1 } else { no };
            // Independent complex spectra include nonzero DC/input Nyquist,
            // all other bins, and a single active bin on either side of each
            // conjugate pair. This exercises every retained bin, not one tone.
            for trial in 0..9 {
                for (k, value) in kernel.input_spectrum.iter_mut().enumerate() {
                    let re = ((k * 17 + trial * 11 + 3) % 97) as f64 / 97.0 - 0.5;
                    let im = if k == 0 || k == ni {
                        0.0
                    } else {
                        ((k * 29 + trial * 7 + 5) % 89) as f64 / 89.0 - 0.5
                    };
                    *value = if trial < 3 || k % 6 == trial - 3 {
                        Complex::new(re, im)
                    } else {
                        Complex::default()
                    };
                }
                spectrum.fill(Complex::default());
                for (k, value) in spectrum[..keep].iter_mut().enumerate() {
                    *value = kernel.input_spectrum[k] * kernel.filter[k];
                }
                inverse
                    .process_with_scratch(&mut spectrum, &mut expected, &mut scratch)
                    .unwrap();
                kernel.prepare_inverse();
                // Separate storage keeps the oracle independent of the
                // production real/complex slice cast.
                let mut actual = vec![Complex::default(); no];
                kernel.inverse.process_outofplace_with_scratch(
                    &mut kernel.output_spectrum,
                    &mut actual,
                    &mut kernel.output_scratch,
                );
                for (pair, reference) in actual.iter().zip(expected.chunks_exact(2)) {
                    for (value, wanted) in [pair.re, pair.im].into_iter().zip(reference) {
                        let error = (value - wanted).abs();
                        maximum_error = maximum_error.max(error);
                        assert!(error <= 3e-13, "{ni}->{no}, trial {trial}: {error}");
                        compared += 1;
                    }
                }
            }
        }
        eprintln!(
            "fused inverse: {compared} scalar outputs, maximum absolute error {maximum_error:e}"
        );
    }
}
