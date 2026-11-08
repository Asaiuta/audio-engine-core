//! Consume a local file through streaming decode, resampling, DSP and metering.
//!
//! Run on a worker thread, not an audio callback. Only the current decoded
//! packet and reusable output block are retained; no whole-track PCM is collected.
//! This example measures the result and does not open an audio device.
//!
//! cargo run --example stream_file -- <local-path> [output-rate-hz]

use audio_engine_core::analysis::LoudnessMeter;
use audio_engine_core::audio_block::{AudioBlockMut, AudioBlockRef};
use audio_engine_core::{
    finish_checked, process_checked, CallbackSpec, ChainFinishPolicy, MediaLocation,
    PlaybackConfig, PlaybackPipeline, ProcessBuffers, ProcessError, ProcessState, StreamingDecoder,
    StreamingResampler,
};

/// Process and measure one produced block without retaining its samples.
fn consume(
    pipeline: &mut PlaybackPipeline,
    meter: &mut LoudnessMeter,
    samples: &mut [f64],
    channels: usize,
) -> Result<(), ProcessError> {
    let progress = pipeline.process(samples)?;
    meter.process(&samples[..progress.produced_frames() * channels])
}

/// Drive public streaming APIs with bounded caller-owned PCM storage.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    const BLOCK_FRAMES: usize = 1024;
    let mut args = std::env::args_os().skip(1);
    let path = args
        .next()
        .ok_or("usage: stream_file <local-path> [output-rate-hz]")?;
    let output_rate = args
        .next()
        .map(|rate| rate.to_string_lossy().parse::<u32>())
        .transpose()?
        .unwrap_or(44_100);
    let mut decoder = StreamingDecoder::open(MediaLocation::local(path))?;
    let channels = decoder.info().channels;
    let source_rate = decoder.info().sample_rate;
    let mut resampler = StreamingResampler::new(channels, source_rate, output_rate)?;
    let (mut pipeline, _controller) =
        PlaybackPipeline::builder(CallbackSpec::new(channels, output_rate, BLOCK_FRAMES)?)
            .configure(PlaybackConfig::transparent().with_volume(0.8))
            .build()?;
    let mut meter = LoudnessMeter::with_layout(&decoder.info().channel_layout, output_rate)?;
    let mut output = vec![0.0; BLOCK_FRAMES * channels];
    let mut input_frames = 0_u64;

    while let Some(packet) = decoder.decode_next_borrowed()? {
        input_frames += (packet.len() / channels) as u64;
        for chunk in packet.chunks(BLOCK_FRAMES * channels) {
            let mut consumed = 0;
            while consumed < chunk.len() / channels {
                let progress = process_checked(
                    &mut resampler,
                    ProcessBuffers::out_of_place(
                        AudioBlockRef::new(&chunk[consumed * channels..], channels)?,
                        AudioBlockMut::new(&mut output, channels)?,
                    )?,
                )?;
                consumed += progress.consumed_frames();
                consume(
                    &mut pipeline,
                    &mut meter,
                    &mut output[..progress.produced_frames() * channels],
                    channels,
                )?;
            }
        }
    }
    loop {
        let progress = finish_checked(&mut resampler, AudioBlockMut::new(&mut output, channels)?)?;
        consume(
            &mut pipeline,
            &mut meter,
            &mut output[..progress.produced_frames() * channels],
            channels,
        )?;
        if progress.state() == ProcessState::Finished {
            break;
        }
    }
    loop {
        let progress =
            pipeline.finish_into_with_policy(&mut output, ChainFinishPolicy::default())?;
        meter.process(&output[..progress.produced_frames() * channels])?;
        if progress.state() == ProcessState::Finished {
            break;
        }
    }
    println!("input_frames={input_frames} source_rate={source_rate} output_frames={} output_rate={output_rate} channels={channels} integrated_lufs={:.3}",
        meter.frames_processed(), meter.integrated_loudness());
    Ok(())
}
