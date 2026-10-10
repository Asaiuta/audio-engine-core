//! Synchronous FFT control. See NOTICE: filter constants are from Rubato.

use super::{gcd, samples, zeroed, Config, InitError, ProcessError, Quality};
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use rustfft::num_complex::Complex64 as Complex;
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
    inverse: std::panic::AssertUnwindSafe<Arc<dyn ComplexToReal<f64>>>,
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
        let mut planner = RealFftPlanner::<f64>::new();
        let forward = planner.plan_fft_forward(2 * geometry.input);
        let inverse = planner.plan_fft_inverse(2 * geometry.output);
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
            output_time: inverse.make_output_vec(),
            output_spectrum: inverse.make_input_vec(),
            output_scratch: inverse.make_scratch_vec(),
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
            let keep = if ni < no { ni + 1 } else { no };
            for ((y, x), h) in self.output_spectrum[..keep]
                .iter_mut()
                .zip(&self.input_spectrum)
                .zip(&self.filter)
            {
                *y = *x * *h;
            }
            self.output_spectrum[keep..].fill(Complex::default());
            self.inverse
                .process_with_scratch(
                    &mut self.output_spectrum,
                    &mut self.output_time,
                    &mut self.output_scratch,
                )
                .map_err(|_| ProcessError::new("owned FFT transform failed"))?;
            let overlap = &mut self.overlap[ch * no..(ch + 1) * no];
            for (i, old) in overlap.iter().enumerate() {
                output[i * channels + ch] = self.output_time[i] + old;
            }
            overlap.copy_from_slice(&self.output_time[no..]);
        }
        Ok(())
    }
}
