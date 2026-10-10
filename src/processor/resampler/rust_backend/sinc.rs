//! Fixed-ratio sinc compatibility with output-driven bounded storage.
//!
//! Coefficients, interpolation and virtual block clock follow Rubato 4.0.0
//! under its MIT license (see NOTICE). Scheduling and history are separate:
//! no Rubato calls, full-input staging, output queue or dynamic callback storage.
use super::super::aligned_buffer::AlignedBuffer;
use super::{clock::BlockClock, dot::Dot, zeroed, Config, InitError, ProcessError, Quality};
use std::f64::consts::PI;
use windowfunctions::{window, Symmetry, WindowFunction};

fn cutoff(length: usize, config: Config) -> f64 {
    let n = length as f32;
    let mut c = 1.0
        / (13.745_202_940_783_823_f64 as f32 / n
            + 121.735_325_863_749_34_f64 as f32 / (n * n)
            + 5_964.163_279_612_051_f64 as f32 / (n * n * n)
            + 1.0);
    if config.to < config.from {
        c *= (config.to as f64 / config.from as f64) as f32;
    }
    f64::from(c)
}

struct Position {
    starts: [u64; 4],
    rows: [usize; 4],
    fraction: f64,
    end: u64,
}

/// Interpolation asks only for rows -1 through phases + 1. Every configured
/// bank is a power of two, so boundary wrapping needs no integer division.
#[inline]
fn bank_position(row: i64, phases: usize) -> (i64, usize) {
    let shift = if row < 0 {
        -1
    } else {
        i64::from(row >= phases as i64)
    };
    (shift, row as usize & (phases - 1))
}

#[inline]
fn absolute_origin(base: u64, length: usize, integral: i64) -> Result<u64, ProcessError> {
    if let Some(offset) = integral.checked_add((2 * length) as i64) {
        return base
            .checked_add_signed(offset)
            .ok_or(ProcessError::new("owned sinc sample position overflow"));
    }
    u64::try_from(base as i128 + (2 * length) as i128 + integral as i128)
        .map_err(|_| ProcessError::new("owned sinc sample position overflow"))
}

pub(super) struct SincStream {
    config: Config,
    length: usize,
    phases: usize,
    points: usize,
    ratio: f64,
    step: f64,
    coefficients: AlignedBuffer,
    combined: AlignedBuffer,
    history: Vec<f64>,
    capacity: usize,
    written: u64,
    accepted: u64,
    emitted: u64,
    initial_clock: BlockClock,
    clock: BlockClock,
    dot: Dot,
    ending: bool,
}

impl SincStream {
    pub(super) fn new(config: Config) -> Result<Self, InitError> {
        config.validate()?;
        let (length, phases, points) = match config.quality {
            Quality::Low => (64, 128, 2),
            Quality::Standard => (128, 256, 4),
            Quality::High => (256, 256, 4),
            Quality::UltraHigh => (256, 512, 4),
        };
        let total = length * phases;
        let fc = cutoff(length, config);
        let mut sum = 0.0;
        let values: Vec<_> =
            window::<f64>(total, WindowFunction::BlackmanHarris, Symmetry::Periodic)
                .enumerate()
                .map(|(i, w)| {
                    let x = (i as f64 - (total / 2) as f64) * fc / phases as f64;
                    let sinc = if x == 0.0 {
                        1.0
                    } else {
                        (x * PI).sin() / (x * PI)
                    };
                    let value = (w * w) * sinc;
                    sum += value;
                    value
                })
                .collect();
        sum /= phases as f64;
        let mut coefficients = AlignedBuffer::zeroed(total)?;
        for tap in 0..length {
            for row in 0..phases {
                coefficients[(phases - row - 1) * length + tap] = values[phases * tap + row] / sum;
            }
        }
        let ratio = config.to as f64 / config.from as f64;
        let step = 1.0 / ratio;
        let clock = BlockClock::initial(length, ratio);
        let capacity = (2 * length + 8).next_power_of_two();
        let history = zeroed(2 * capacity, config.channels, "sinc history")?;
        Ok(Self {
            config,
            length,
            phases,
            points,
            ratio,
            step,
            coefficients,
            combined: AlignedBuffer::zeroed(length + 1)?,
            history,
            capacity,
            written: (2 * length) as u64,
            accepted: 0,
            emitted: 0,
            initial_clock: clock,
            clock,
            dot: Dot::sinc_compatible(),
            ending: false,
        })
    }

    #[inline(always)]
    fn position<const POINTS: usize>(&mut self) -> Result<Position, ProcessError> {
        self.clock.prepare(self.length, self.ratio)?;
        let next = self.clock.index + self.step;
        let integral = next.floor() as i64;
        let cell = ((next - next.floor()) * self.phases as f64).floor() as i64;
        // The same absolute origin is shared by every interpolation row.
        // Ordinary positions use checked machine-word arithmetic; retain the
        // wide fallback for cancellation at extreme counter boundaries.
        let origin = absolute_origin(self.clock.base, self.length, integral)?;
        let offset = if POINTS == 4 { -1 } else { 0 };
        let mut starts = [0; 4];
        let mut rows = [0; 4];
        for j in 0..POINTS {
            let r = cell + offset + j as i64;
            let (delta, row) = bank_position(r, self.phases);
            rows[j] = row;
            starts[j] = origin
                .checked_add_signed(delta)
                .ok_or(ProcessError::new("owned sinc sample position overflow"))?;
        }
        let end = starts[POINTS - 1]
            .checked_add(self.length as u64)
            .ok_or(ProcessError::new("owned sinc window overflow"))?;
        let scaled = next * self.phases as f64;
        Ok(Position {
            starts,
            rows,
            fraction: scaled - scaled.floor(),
            end,
        })
    }

    fn write_input(&mut self, input: &[f64], frames: usize) -> Result<(), ProcessError> {
        let channels = self.config.channels;
        let next_written = self
            .written
            .checked_add(frames as u64)
            .ok_or(ProcessError::new("owned sinc input position overflow"))?;
        let next_accepted = self
            .accepted
            .checked_add(frames as u64)
            .ok_or(ProcessError::new("owned sinc input count overflow"))?;
        let mut copied = 0;
        while copied < frames {
            let slot = ((self.written + copied as u64) & (self.capacity - 1) as u64) as usize;
            let count = (frames - copied).min(self.capacity - slot);
            let source = &input[copied * channels..(copied + count) * channels];
            for channel in 0..channels {
                let base = channel * 2 * self.capacity;
                let (lower, upper) =
                    self.history[base..base + 2 * self.capacity].split_at_mut(self.capacity);
                let lower = &mut lower[slot..slot + count];
                let upper = &mut upper[slot..slot + count];
                if channels == 1 {
                    lower.copy_from_slice(source);
                    upper.copy_from_slice(source);
                    continue;
                } else if channels == 2 {
                    for (target, frame) in lower.iter_mut().zip(source.chunks_exact(2)) {
                        *target = frame[channel];
                    }
                } else if channels == 6 {
                    for (target, frame) in lower.iter_mut().zip(source.chunks_exact(6)) {
                        *target = frame[channel];
                    }
                } else {
                    for (target, frame) in lower.iter_mut().zip(source.chunks_exact(channels)) {
                        *target = frame[channel];
                    }
                }
                // Mirror the contiguous channel once; the deinterleave loop
                // has one destination and fixed strides for common layouts.
                upper.copy_from_slice(lower);
            }
            copied += count;
        }
        self.written = next_written;
        self.accepted = next_accepted;
        Ok(())
    }

    fn pad_to(&mut self, end: u64) {
        // A sparse-output conversion can skip millions of silent input frames.
        // Only one fixed history ring can affect the next output.
        if end.saturating_sub(self.written) >= self.capacity as u64 {
            self.history.fill(0.0);
            self.written = end;
            return;
        }
        while self.written < end {
            let slot = (self.written & (self.capacity - 1) as u64) as usize;
            for channel in 0..self.config.channels {
                let base = channel * 2 * self.capacity + slot;
                self.history[base] = 0.0;
                self.history[base + self.capacity] = 0.0;
            }
            self.written += 1;
        }
    }

    fn sample(&self, channel: usize, start: u64, filter: &[f64]) -> f64 {
        let base = channel * 2 * self.capacity + (start & (self.capacity - 1) as u64) as usize;
        self.dot
            .sample(&self.history[base..base + self.length], filter)
    }

    #[inline(always)]
    fn emit<const POINTS: usize, const COMBINED: bool>(
        &mut self,
        p: &Position,
        output: &mut [f64],
    ) -> Result<(), ProcessError> {
        if p.end > self.written || p.starts[0] < self.written.saturating_sub(self.capacity as u64) {
            return Err(ProcessError::new("owned sinc history interval unavailable"));
        }
        let channels = self.config.channels;
        let x = p.fraction;
        if COMBINED {
            let weights = if POINTS == 4 {
                let x2 = x * x;
                let x3 = x2 * x;
                [
                    -1.0 / 3.0 * x + 0.5 * x2 - 1.0 / 6.0 * x3,
                    1.0 - 0.5 * x - x2 + 0.5 * x3,
                    x + 0.5 * x2 - 0.5 * x3,
                    -1.0 / 6.0 * x + 1.0 / 6.0 * x3,
                ]
            } else {
                [1.0 - x, x, 0.0, 0.0]
            };
            let mut filters = [&[][..]; 4];
            let mut shifts = [0; 4];
            for j in 0..POINTS {
                let row = p.rows[j] * self.length;
                filters[j] = &self.coefficients[row..row + self.length];
                shifts[j] = (p.starts[j] - p.starts[0]) as usize;
            }
            self.dot
                .combine(filters, shifts, weights, POINTS, &mut self.combined);
            for (channel, sample) in output.iter_mut().enumerate().take(channels) {
                let base = channel * 2 * self.capacity
                    + (p.starts[0] & (self.capacity - 1) as u64) as usize;
                *sample = self.sample(channel, p.starts[0], &self.combined[..self.length])
                    + self.combined[self.length] * self.history[base + self.length];
            }
        } else {
            for (channel, sample) in output.iter_mut().enumerate().take(channels) {
                if POINTS == 2 {
                    let base = channel * 2 * self.capacity
                        + (p.starts[0] & (self.capacity - 1) as u64) as usize;
                    let shift = (p.starts[1] - p.starts[0]) as usize;
                    let row0 = p.rows[0] * self.length;
                    let row1 = p.rows[1] * self.length;
                    let y = self.dot.sample_pair(
                        &self.history[base..base + self.length + shift],
                        &self.coefficients[row0..row0 + self.length],
                        &self.coefficients[row1..row1 + self.length],
                        shift,
                    );
                    *sample = y[0] + x * (y[1] - y[0]);
                    continue;
                }
                let mut y = [0.0; 4];
                for (j, value) in y.iter_mut().enumerate().take(POINTS) {
                    let row = p.rows[j] * self.length;
                    *value = self.sample(
                        channel,
                        p.starts[j],
                        &self.coefficients[row..row + self.length],
                    );
                }
                *sample = if POINTS == 2 {
                    y[0] + x * (y[1] - y[0])
                } else {
                    let a1 = -1.0 / 3.0 * y[0] - 0.5 * y[1] + y[2] - 1.0 / 6.0 * y[3];
                    let a2 = 0.5 * (y[0] + y[2]) - y[1];
                    let a3 = 0.5 * (y[1] - y[2]) + 1.0 / 6.0 * (y[3] - y[0]);
                    let x2 = x * x;
                    let x3 = x2 * x;
                    y[1] + a1 * x + a2 * x2 + a3 * x3
                };
            }
        }
        self.clock.advance(self.step);
        self.emitted = self
            .emitted
            .checked_add(1)
            .ok_or(ProcessError::new("owned sinc output count overflow"))?;
        Ok(())
    }
}

impl SincStream {
    pub(super) fn reset(&mut self) -> Result<(), ProcessError> {
        self.history.fill(0.0);
        self.written = (2 * self.length) as u64;
        self.accepted = 0;
        self.emitted = 0;
        self.clock = self.initial_clock;
        self.ending = false;
        Ok(())
    }

    pub(super) fn process(
        &mut self,
        input: &[f64],
        output: &mut [f64],
    ) -> Result<super::BackendProgress, ProcessError> {
        match (self.points, self.config.channels) {
            (2, 1 | 2) => self.process_inner::<2, false>(input, output),
            (2, _) => self.process_inner::<2, true>(input, output),
            (_, 1) => self.process_inner::<4, false>(input, output),
            _ => self.process_inner::<4, true>(input, output),
        }
    }

    fn process_inner<const POINTS: usize, const COMBINED: bool>(
        &mut self,
        input: &[f64],
        output: &mut [f64],
    ) -> Result<super::BackendProgress, ProcessError> {
        let c = self.config.channels;
        if self.ending || !input.len().is_multiple_of(c) || !output.len().is_multiple_of(c) {
            return Err(ProcessError::new(
                "invalid owned sinc process buffers/state",
            ));
        }
        let (mut read, mut produced) = (0, 0);
        let mut budget = self.config.output_frames(self.accepted)?;
        while produced < output.len() / c {
            let p = self.position::<POINTS>()?;
            // A position does not change while waiting for its input/budget.
            // Fill that requirement without recomputing the floating clock,
            // interpolation rows and checked absolute positions a second time.
            while (p.end > self.written || self.emitted >= budget) && read < input.len() / c {
                // Batch only into free history after the oldest sample needed
                // by this pending output. This keeps the same fixed ring and
                // never overwrites an interpolation window awaiting emission.
                let available = p.starts[0]
                    .saturating_add(self.capacity as u64)
                    .saturating_sub(self.written)
                    .min((input.len() / c - read) as u64);
                if available == 0 {
                    return Err(ProcessError::new(
                        "owned sinc prefill would overwrite pending history",
                    ));
                }
                let frames = available as usize;
                self.write_input(&input[read * c..(read + frames) * c], frames)?;
                read += frames;
                budget = self.config.output_frames(self.accepted)?;
            }
            if p.end > self.written || self.emitted >= budget {
                break;
            }
            self.emit::<POINTS, COMBINED>(&p, &mut output[produced * c..(produced + 1) * c])?;
            produced += 1;
        }
        Ok(super::BackendProgress {
            input_frames: read,
            output_frames: produced,
        })
    }

    pub(super) fn finish(&mut self, output: &mut [f64]) -> Result<usize, ProcessError> {
        match (self.points, self.config.channels) {
            (2, 1 | 2) => self.finish_inner::<2, false>(output),
            (2, _) => self.finish_inner::<2, true>(output),
            (_, 1) => self.finish_inner::<4, false>(output),
            _ => self.finish_inner::<4, true>(output),
        }
    }

    fn finish_inner<const POINTS: usize, const COMBINED: bool>(
        &mut self,
        output: &mut [f64],
    ) -> Result<usize, ProcessError> {
        let c = self.config.channels;
        if !output.len().is_multiple_of(c) {
            return Err(ProcessError::new("invalid owned sinc finish buffer"));
        }
        self.ending = true;
        let target = self.config.output_frames(self.accepted)?;
        let mut produced = 0;
        while produced < output.len() / c && self.emitted < target {
            let p = self.position::<POINTS>()?;
            self.pad_to(p.end);
            self.emit::<POINTS, COMBINED>(&p, &mut output[produced * c..(produced + 1) * c])?;
            produced += 1;
        }
        Ok(produced)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sinc_storage_is_aligned_and_keeps_the_requested_slice_length() {
        for length in [1, 65, 129, 257, 128 * 64, 512 * 256] {
            let mut storage = AlignedBuffer::zeroed(length).unwrap();
            assert_eq!(storage.len(), length);
            if std::mem::align_of::<f64>() >= 8 {
                assert_eq!(storage.as_ptr() as usize % 32, 0);
            }
            storage.fill(0.75);
            assert!(storage.iter().all(|value| *value == 0.75));
        }
    }

    fn render(config: Config, input: &[f64], capacity: usize) -> Vec<f64> {
        let mut stream = SincStream::new(config).unwrap();
        let mut scratch = vec![0.0; capacity * config.channels];
        let mut output = Vec::new();
        let mut read = 0;
        while read < input.len() / config.channels {
            let step = stream
                .process(&input[read * config.channels..], &mut scratch)
                .unwrap();
            assert!(step.input_frames != 0 || step.output_frames != 0);
            read += step.input_frames;
            output.extend_from_slice(&scratch[..step.output_frames * config.channels]);
        }
        loop {
            let count = stream.finish(&mut scratch).unwrap();
            output.extend_from_slice(&scratch[..count * config.channels]);
            if count == 0 {
                break;
            }
        }
        let frames = input.len() / config.channels;
        assert_eq!(
            output.len(),
            ((frames as u128 * config.to as u128 * 2 + config.from as u128)
                / (config.from as u128 * 2)) as usize
                * config.channels
        );
        output
    }

    #[test]
    fn prefilled_history_wraps_preserve_complete_one_frame_output() {
        for (from, to, frames) in [(44101, 44100, 2053), (48000, 1001, 8193), (1001, 48000, 73)] {
            for quality in [Quality::Low, Quality::UltraHigh] {
                for channels in [1, 2, 6] {
                    let config = Config {
                        from,
                        to,
                        quality,
                        channels,
                    };
                    let input: Vec<_> = (0..frames * channels)
                        .map(|n| ((n * 37 % 101) as f64 - 50.0) / 64.0)
                        .collect();
                    let narrow = render(config, &input, 1);
                    let wide = render(config, &input, 1031);
                    assert!(narrow
                        .iter()
                        .zip(&wide)
                        .all(|(a, b)| a.to_bits() == b.to_bits()));
                }
            }
        }
    }

    #[test]
    fn bounded_bank_wrap_matches_euclidean_division_at_every_phase() {
        for phases in [128, 256, 512] {
            for row in -1..=phases as i64 + 1 {
                assert_eq!(
                    bank_position(row, phases),
                    (
                        row.div_euclid(phases as i64),
                        row.rem_euclid(phases as i64) as usize,
                    )
                );
            }
        }
    }
}

#[cfg(test)]
mod position_boundary_tests {
    use super::*;

    #[test]
    fn checked_position_matches_wide_oracle_at_counter_boundaries() {
        for length in [64, 128, 256] {
            for base in [0, 1, 1024, u64::MAX / 2, u64::MAX - 1024, u64::MAX] {
                for integral in [i64::MIN, -4096, -513, -256, -1, 0, 1, 4096, i64::MAX] {
                    let expected =
                        u64::try_from(base as i128 + 2 * length as i128 + integral as i128);
                    match expected {
                        Ok(value) => {
                            assert_eq!(absolute_origin(base, length, integral).unwrap(), value)
                        }
                        Err(_) => assert!(absolute_origin(base, length, integral).is_err()),
                    }
                }
            }
        }
    }
}
