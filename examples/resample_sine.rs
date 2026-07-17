//! Offline resampling of a synthetic sine wave.
//!
//! This example needs no audio files and no optional features. It generates a
//! mono 48 kHz sine, streams it through the SoX VHQ engine to 44.1 kHz in
//! fixed-size chunks, then finishes the stream and prints the frame counts
//! so you can verify the ratio.
//!
//! `StreamingResampler` can partially consume input or fill output. Advance
//! both cursors from `ProcessProgress`, then call `finish_checked` until it
//! reaches `Finished`.
//!
//! Run with:
//!
//! ```text
//! cargo run --example resample_sine
//! ```

use audio_engine_core::{
    finish_checked, process_checked, AudioBlockMut, AudioBlockRef, ProcessBuffers, ProcessState,
    StreamingResampler,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    const FROM_RATE: u32 = 48_000;
    const TO_RATE: u32 = 44_100;
    const CHANNELS: usize = 1;
    const FREQ_HZ: f64 = 440.0;
    const DURATION_SECS: f64 = 1.0;
    const CHUNK_FRAMES: usize = 4_096;

    // Synthesize one second of a 440 Hz sine at the source rate.
    let input_frames = (FROM_RATE as f64 * DURATION_SECS) as usize;
    let mut input = Vec::with_capacity(input_frames);
    for n in 0..input_frames {
        let t = n as f64 / FROM_RATE as f64;
        input.push((2.0 * std::f64::consts::PI * FREQ_HZ * t).sin());
    }

    let mut resampler = StreamingResampler::new(CHANNELS, FROM_RATE, TO_RATE)?;

    // Stream the signal through the resampler one chunk at a time, advancing
    // exact input/output progress on every call.
    let mut output: Vec<f64> = Vec::new();
    let mut scratch = vec![0.0; CHUNK_FRAMES * CHANNELS];
    for chunk in input.chunks(CHUNK_FRAMES * CHANNELS) {
        let mut consumed_frames = 0;
        let chunk_frames = chunk.len() / CHANNELS;
        while consumed_frames < chunk_frames {
            let input_block = AudioBlockRef::new(&chunk[consumed_frames * CHANNELS..], CHANNELS)?;
            let output_block = AudioBlockMut::new(&mut scratch, CHANNELS)?;
            let progress = process_checked(
                &mut resampler,
                ProcessBuffers::out_of_place(input_block, output_block)?,
            )?;
            consumed_frames += progress.consumed_frames();
            output.extend_from_slice(&scratch[..progress.produced_frames() * CHANNELS]);
        }
    }
    loop {
        let output_block = AudioBlockMut::new(&mut scratch, CHANNELS)?;
        let progress = finish_checked(&mut resampler, output_block)?;
        output.extend_from_slice(&scratch[..progress.produced_frames() * CHANNELS]);
        if progress.state() == ProcessState::Finished {
            break;
        }
    }

    let expected = (input_frames as f64 * TO_RATE as f64 / FROM_RATE as f64).round() as usize;
    println!(
        "resampled {} frames @ {} Hz -> {} frames @ {} Hz (expected ~{})",
        input_frames,
        FROM_RATE,
        output.len(),
        TO_RATE,
        expected
    );

    assert_eq!(output.len(), expected);

    Ok(())
}
