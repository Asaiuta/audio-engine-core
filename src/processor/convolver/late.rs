//! A delayed, larger partition tier. The IR starts two periods after frame zero:
//! one period captures input, the next computes its output before it is needed.
//! Each quantum performs one channel FFT, one partition MAC, or one channel IFFT.
//! Scheduling depends only on frames, never on the caller's chunk boundaries.

use super::{accumulate_spectrum_in_place, Complex, ProcessError, RealFftPlanner};
use realfft::{ComplexToReal, RealToComplex};
use std::sync::Arc;

pub(super) const PARTITION: usize = 4096;
pub(super) const IR_OFFSET: usize = 2 * PARTITION;
pub(super) const MIN_IR_FRAMES: usize = 16 * PARTITION;
// Keep late transforms away from both sides of each early-tier boundary.
// A transform receives four MAC quanta of spacing, preventing adjacent channel
// IFFT/FFT jobs from bunching up in a small callback.
const BASE_PERIOD: usize = super::PARTITIONED_CONVOLUTION_PARTITION_SIZE;
const GUARD: usize = 128;
const SCHEDULE_FRAMES: usize = PARTITION / BASE_PERIOD * (BASE_PERIOD - 2 * GUARD);
const TRANSFORM_WEIGHT: usize = 4;

#[derive(Clone)]
pub(super) struct LateTail {
    states: Vec<Channel>,
    partitions: usize,
    forward: Arc<dyn RealToComplex<f64>>,
    inverse: Arc<dyn ComplexToReal<f64>>,
    real: Vec<f64>,
    spectrum: Vec<Complex<f64>>,
    scratch: Vec<Complex<f64>>,
    position: usize,
    cursor: usize,
    valid_slots: usize,
    active: bool,
    schedule_error: usize,
    work_per_period: usize,
    job_channel: usize,
    job_stage: usize,
    history_slot: usize,
}

#[derive(Clone)]
struct Channel {
    ir: Vec<Complex<f64>>,
    history: Vec<Complex<f64>>,
    capture: Vec<f64>,
    captured: Vec<f64>,
    output: Vec<f64>,
    pending: Vec<f64>,
    overlap: Vec<f64>,
    accumulator: Vec<Complex<f64>>,
}

impl LateTail {
    pub(super) fn new(ir: &[f64], channels: usize) -> Result<Self, ProcessError> {
        let frames = ir.len() / channels;
        let partitions = (frames - IR_OFFSET).div_ceil(PARTITION);
        let mut planner = RealFftPlanner::<f64>::new();
        let forward = planner.plan_fft_forward(2 * PARTITION);
        let inverse = planner.plan_fft_inverse(2 * PARTITION);
        let bins = forward.complex_len();
        let mut real = vec![0.0; 2 * PARTITION];
        let mut scratch =
            vec![Complex::new(0.0, 0.0); forward.get_scratch_len().max(inverse.get_scratch_len())];
        let mut states = Vec::with_capacity(channels);
        for channel in 0..channels {
            let mut spectra = vec![Complex::new(0.0, 0.0); partitions * bins];
            for partition in 0..partitions {
                real.fill(0.0);
                let start = IR_OFFSET + partition * PARTITION;
                for frame in 0..(frames - start).min(PARTITION) {
                    real[frame] = ir[(start + frame) * channels + channel];
                }
                forward
                    .process_with_scratch(
                        &mut real,
                        &mut spectra[partition * bins..(partition + 1) * bins],
                        &mut scratch,
                    )
                    .map_err(|_| backend_error("construct late partition spectra"))?;
            }
            states.push(Channel {
                ir: spectra,
                history: vec![Complex::new(0.0, 0.0); partitions * bins],
                capture: vec![0.0; PARTITION],
                captured: vec![0.0; PARTITION],
                output: vec![0.0; PARTITION],
                pending: vec![0.0; PARTITION],
                overlap: vec![0.0; PARTITION],
                accumulator: vec![Complex::new(0.0, 0.0); bins],
            });
        }
        Ok(Self {
            states,
            partitions,
            forward,
            inverse,
            real,
            spectrum: vec![Complex::new(0.0, 0.0); bins],
            scratch,
            position: 0,
            cursor: 0,
            valid_slots: 0,
            active: false,
            schedule_error: 0,
            work_per_period: channels * (partitions + 2 * TRANSFORM_WEIGHT),
            job_channel: 0,
            job_stage: 0,
            history_slot: 0,
        })
    }

    pub(super) fn reset(&mut self) {
        self.position = 0;
        self.cursor = 0;
        self.valid_slots = 0;
        self.active = false;
        self.schedule_error = 0;
        self.job_channel = 0;
        self.job_stage = 0;
        self.history_slot = 0;
        for state in &mut self.states {
            state.output.fill(0.0);
            state.pending.fill(0.0);
            state.overlap.fill(0.0);
            state.accumulator.fill(Complex::new(0.0, 0.0));
        }
        // IR-sized history is invalidated, not cleared. Capture buffers are
        // completely overwritten before the first post-reset FFT can read them.
    }

    pub(super) fn process_into(
        &mut self,
        input: &[f64],
        output: &mut [f64],
    ) -> Result<(), ProcessError> {
        let channels = self.states.len();
        for (input, output) in input
            .chunks_exact(channels)
            .zip(output.chunks_exact_mut(channels))
        {
            for ((state, &input), output) in self.states.iter_mut().zip(input).zip(output) {
                state.capture[self.position] = input;
                *output += state.output[self.position];
            }
            let base_position = self.position % BASE_PERIOD;
            if self.active && (GUARD..BASE_PERIOD - GUARD).contains(&base_position) {
                self.schedule_error += self.work_per_period;
                loop {
                    let weight = if self.job_stage == 0 || self.job_stage == self.partitions + 1 {
                        TRANSFORM_WEIGHT
                    } else {
                        1
                    };
                    if self.schedule_error < SCHEDULE_FRAMES * weight {
                        break;
                    }
                    self.schedule_error -= SCHEDULE_FRAMES * weight;
                    self.run_quantum()?;
                }
            }
            self.position += 1;
            if self.position == PARTITION {
                self.advance_period();
            }
        }
        Ok(())
    }

    fn advance_period(&mut self) {
        if self.active {
            debug_assert_eq!(self.job_channel, self.states.len());
            debug_assert_eq!(self.job_stage, 0);
            debug_assert_eq!(self.schedule_error, 0);
            self.valid_slots = (self.valid_slots + 1).min(self.partitions);
            self.cursor += 1;
            if self.cursor == self.partitions {
                self.cursor = 0;
            }
        }
        for state in &mut self.states {
            std::mem::swap(&mut state.capture, &mut state.captured);
            std::mem::swap(&mut state.output, &mut state.pending);
        }
        self.position = 0;
        self.active = true;
        self.job_channel = 0;
        self.job_stage = 0;
        self.history_slot = self.cursor;
    }

    fn run_quantum(&mut self) -> Result<(), ProcessError> {
        let state = &mut self.states[self.job_channel];
        let bins = self.spectrum.len();
        if self.job_stage == 0 {
            self.real[..PARTITION].copy_from_slice(&state.captured);
            self.real[PARTITION..].fill(0.0);
            self.forward
                .process_with_scratch(&mut self.real, &mut self.spectrum, &mut self.scratch)
                .map_err(|_| backend_error("forward late partition FFT"))?;
            state.history[self.cursor * bins..(self.cursor + 1) * bins]
                .copy_from_slice(&self.spectrum);
            self.history_slot = self.cursor;
        } else if self.job_stage <= self.partitions {
            let partition = self.job_stage - 1;
            if partition == 0 || self.history_slot < self.valid_slots {
                let input_start = self.history_slot * bins;
                let ir_start = partition * bins;
                accumulate_spectrum_in_place(
                    &mut state.accumulator,
                    &state.history[input_start..input_start + bins],
                    &state.ir[ir_start..ir_start + bins],
                );
            }
            self.history_slot = if self.history_slot == 0 {
                self.partitions - 1
            } else {
                self.history_slot - 1
            };
        } else {
            self.inverse
                .process_with_scratch(&mut state.accumulator, &mut self.real, &mut self.scratch)
                .map_err(|_| backend_error("inverse late partition FFT"))?;
            state.accumulator.fill(Complex::new(0.0, 0.0));
            let scale = 1.0 / (2 * PARTITION) as f64;
            for frame in 0..PARTITION {
                state.pending[frame] = self.real[frame] * scale + state.overlap[frame];
                state.overlap[frame] = self.real[frame + PARTITION] * scale;
            }
        }
        self.job_stage += 1;
        if self.job_stage == self.partitions + 2 {
            self.job_stage = 0;
            self.job_channel += 1;
        }
        Ok(())
    }
}

fn backend_error(operation: &'static str) -> ProcessError {
    ProcessError::Backend {
        processor: "FFTConvolver",
        operation,
        message: "real FFT buffer invariant failed",
    }
}
