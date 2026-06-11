//! Offline resampling of a synthetic sine wave.
//!
//! This example needs no audio files and no optional features. It generates a
//! mono 48 kHz sine, streams it through the SoX VHQ engine to 44.1 kHz in
//! fixed-size chunks, then flushes the filter tail and prints the frame counts
//! so you can verify the ratio.
//!
//! `StreamingResampler` buffers input internally and emits resampled audio in
//! batches, so the idiomatic pattern is to feed successive chunks and append
//! whatever each call returns, then `flush_into` once the input is exhausted.
//!
//! Run with:
//!
//! ```text
//! cargo run --example resample_sine
//! ```

use audio_engine_core::StreamingResampler;

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

    // Stream the signal through the resampler one chunk at a time, then flush
    // the filter tail to recover the last buffered samples.
    let mut output: Vec<f64> = Vec::new();
    for chunk in input.chunks(CHUNK_FRAMES * CHANNELS) {
        resampler.process_chunk_append(chunk, &mut output);
    }
    resampler.flush_into(&mut output);

    let expected = (input_frames as f64 * TO_RATE as f64 / FROM_RATE as f64).round() as usize;
    println!(
        "resampled {} frames @ {} Hz -> {} frames @ {} Hz (expected ~{})",
        input_frames,
        FROM_RATE,
        output.len(),
        TO_RATE,
        expected
    );

    // The output is shorter than the ideal ratio by the resampler's group
    // delay (the VHQ polyphase filter has ~1 ms of latency). Allow a margin
    // that comfortably covers that filter tail over a one-second signal.
    assert!(
        output.len().abs_diff(expected) <= 1_024,
        "output frame count {} should be within the filter-tail margin of {}",
        output.len(),
        expected
    );

    Ok(())
}
