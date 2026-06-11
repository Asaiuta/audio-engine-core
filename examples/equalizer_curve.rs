//! Applying a 10-band graphic equalizer to a synthetic signal.
//!
//! This example needs no audio files and no optional features. It builds a
//! two-channel buffer, configures a bass-boost / treble-cut curve across the
//! 10 EQ bands, and processes the buffer in place.
//!
//! Run with:
//!
//! ```text
//! cargo run --example equalizer_curve
//! ```

use audio_engine_core::Equalizer;

fn main() {
    const SAMPLE_RATE: f64 = 48_000.0;
    const CHANNELS: usize = 2;
    const FRAMES: usize = 4_800; // 100 ms

    let mut eq = Equalizer::new(CHANNELS, SAMPLE_RATE);

    // Ten band gains in dB, from low to high frequency: lift the lows, leave
    // the mids flat, gently roll off the highs.
    let gains: [f64; 10] = [6.0, 4.0, 2.0, 0.0, 0.0, 0.0, 0.0, -2.0, -4.0, -6.0];
    eq.set_all_bands(&gains, SAMPLE_RATE);
    eq.set_enabled(true);

    // A simple full-scale impulse train as test input (interleaved stereo).
    let mut buffer = vec![0.0f64; FRAMES * CHANNELS];
    for frame in 0..FRAMES {
        if frame % 480 == 0 {
            buffer[frame * CHANNELS] = 0.5;
            buffer[frame * CHANNELS + 1] = 0.5;
        }
    }

    eq.process(&mut buffer);

    let peak = buffer.iter().fold(0.0f64, |acc, &s| acc.max(s.abs()));
    println!(
        "processed {} stereo frames through a 10-band EQ; output peak = {:.4}",
        FRAMES, peak
    );
    assert!(peak.is_finite());
}
