use std::path::Path;

use symphonia::core::audio::{Channels, SampleBuffer, SignalSpec};
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::formats::FormatOptions;
use symphonia::core::meta::MetadataOptions;

use super::error::{DecodeCancelToken, DecoderError};
use super::metadata::{extract_metadata, merge_metadata_revision, AudioInfo};
use super::source::{
    bytes_to_mib, configured_decode_memory_limit, HttpCredentials, OpenedMediaSource,
    F64_SAMPLE_BYTES,
};
use crate::channel_layout::{ChannelLayout, ChannelPosition};

/// Streaming audio decoder using Symphonia.
///
/// ## Seek contract
///
/// [`StreamingDecoder::seek`] uses Symphonia's `SeekMode::Coarse` only; a
/// sample-exact (`Accurate`) mode is intentionally not exposed. Coarse seeking
/// lands on a packet/frame boundary at or before the requested time, so the
/// post-seek position has bounded inaccuracy. Callers must treat the realized
/// position as "within roughly one packet of the target" rather than
/// sample-exact (see [`StreamingDecoder::SEEK_COARSE_TOLERANCE_FRAMES`]).
pub struct StreamingDecoder {
    format_reader: Box<dyn symphonia::core::formats::FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    track_id: u32,
    pub info: AudioInfo,
    sample_buf: Option<SampleBuffer<f64>>,
    samples_output: u64,
    finished: bool,
    /// True only while the decoder is positioned at the true start of the
    /// stream and the leading `encoder_delay` has not yet been consumed. Set
    /// to `false` once start-delay trimming completes, and crucially is *not*
    /// re-armed by [`StreamingDecoder::seek`] — encoder-delay compensation must
    /// apply at true stream start only, never after an arbitrary seek.
    at_stream_start: bool,
    cancel_token: Option<DecodeCancelToken>,
}

const DEFAULT_MAX_DECODED_PACKET_FRAMES: u64 = 65_536;

/// Probed streaming source awaiting fixed decoder-staging allocation.
pub struct StreamingDecoderBuilder {
    format_reader: Box<dyn symphonia::core::formats::FormatReader>,
    track_id: u32,
    pub info: AudioInfo,
    staging_frames: u64,
    signal_spec: SignalSpec,
    cancel_token: Option<DecodeCancelToken>,
}

impl StreamingDecoderBuilder {
    /// Exact fixed `SampleBuffer<f64>` payload allocated by [`Self::build`].
    pub fn staging_buffer_bytes(&self) -> Result<usize, DecoderError> {
        usize::try_from(self.staging_frames)
            .ok()
            .and_then(|frames| frames.checked_mul(self.info.channels))
            .and_then(|samples| samples.checked_mul(std::mem::size_of::<f64>()))
            .ok_or_else(|| DecoderError::Decoder("decoder staging size overflow".to_string()))
    }

    pub fn build(self) -> Result<StreamingDecoder, DecoderError> {
        if self
            .cancel_token
            .as_ref()
            .is_some_and(DecodeCancelToken::is_cancelled)
        {
            return Err(DecoderError::Canceled);
        }
        let track = self
            .format_reader
            .tracks()
            .iter()
            .find(|track| track.id == self.track_id)
            .ok_or(DecoderError::NoAudioTrack)?;
        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(|error| DecoderError::Decoder(error.to_string()))?;
        let sample_buf = SampleBuffer::new(self.staging_frames, self.signal_spec);

        Ok(StreamingDecoder {
            format_reader: self.format_reader,
            decoder,
            track_id: self.track_id,
            info: self.info,
            sample_buf: Some(sample_buf),
            samples_output: 0,
            finished: false,
            at_stream_start: true,
            cancel_token: self.cancel_token,
        })
    }
}

impl StreamingDecoder {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, DecoderError> {
        Self::open_with_credentials(path, None)
    }

    pub fn open_with_credentials<P: AsRef<Path>>(
        path: P,
        credentials: Option<&HttpCredentials>,
    ) -> Result<Self, DecoderError> {
        Self::open_with_credentials_and_cancel(path, credentials, None)
    }

    pub fn open_with_credentials_and_cancel<P: AsRef<Path>>(
        path: P,
        credentials: Option<&HttpCredentials>,
        cancel_token: Option<DecodeCancelToken>,
    ) -> Result<Self, DecoderError> {
        let source = OpenedMediaSource::open_path_with_credentials_and_cancel(
            path,
            credentials,
            cancel_token.clone(),
        )?;
        Self::from_opened_source(source, cancel_token)
    }

    /// Probe and construct a decoder from an already-opened source.
    ///
    /// This preserves transport/source identity across player-owned lifecycle
    /// transitions and avoids reopening by path after a source factory succeeds.
    pub fn from_opened_source(
        source: OpenedMediaSource,
        cancel_token: Option<DecodeCancelToken>,
    ) -> Result<Self, DecoderError> {
        Self::probe_opened_source(source, cancel_token)?.build()
    }

    pub fn probe_opened_source(
        source: OpenedMediaSource,
        cancel_token: Option<DecodeCancelToken>,
    ) -> Result<StreamingDecoderBuilder, DecoderError> {
        if cancel_token
            .as_ref()
            .is_some_and(DecodeCancelToken::is_cancelled)
        {
            return Err(DecoderError::Canceled);
        }

        let OpenedMediaSource { stream, hint } = source;

        let format_opts = FormatOptions::default();
        let metadata_opts = MetadataOptions::default();
        let mut probed = symphonia::default::get_probe()
            .format(&hint, stream, &format_opts, &metadata_opts)
            .map_err(map_probe_error)?;

        let mut metadata = extract_metadata(&mut probed);

        let mut format_reader = probed.format;
        if let Some(revision) = format_reader.metadata().current() {
            merge_metadata_revision(&mut metadata, revision);
        }

        let track = format_reader
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or(DecoderError::NoAudioTrack)?;

        let track_id = track.id;
        let codec_params = &track.codec_params;
        let sample_rate = codec_params.sample_rate.unwrap_or(44100);
        let channels = codec_params.channels.map(|c| c.count()).unwrap_or(2);
        let signal_channels = codec_params
            .channels
            .unwrap_or(Channels::FRONT_LEFT | Channels::FRONT_RIGHT);
        let channel_layout = layout_from_codec(codec_params.channels, channels);
        let bits_per_sample = codec_params.bits_per_sample;
        let total_frames = codec_params.n_frames;
        let duration_secs = total_frames.map(|f| f as f64 / sample_rate as f64);
        let encoder_delay = codec_params.delay.unwrap_or(0);
        let end_padding = codec_params.padding.unwrap_or(0);

        if encoder_delay > 0 || end_padding > 0 {
            log::debug!(
                "Codec delay compensation: delay={}, padding={} samples",
                encoder_delay,
                end_padding
            );
        }

        let info = AudioInfo {
            sample_rate,
            channels,
            channel_layout,
            bits_per_sample,
            total_frames,
            duration_secs,
            encoder_delay,
            end_padding,
            metadata,
        };

        let staging_frames = codec_params
            .max_frames_per_packet
            .filter(|frames| *frames > 0)
            .or(codec_params.frames_per_block.filter(|frames| *frames > 0))
            .unwrap_or(DEFAULT_MAX_DECODED_PACKET_FRAMES);

        log::info!(
            "Opened audio source: {} Hz, {} ch, {:?}s",
            sample_rate,
            channels,
            duration_secs
        );

        Ok(StreamingDecoderBuilder {
            format_reader,
            track_id,
            info,
            staging_frames,
            signal_spec: SignalSpec::new(sample_rate, signal_channels),
            cancel_token,
        })
    }

    /// Documented post-seek position tolerance, in frames.
    ///
    /// [`StreamingDecoder::seek`] is `SeekMode::Coarse`, which lands at or
    /// before the requested time on a packet boundary. The realized first-frame
    /// position after a seek may therefore differ from the exact target by up
    /// to (roughly) one packet. Tests assert the realized position falls within
    /// this many frames of the requested target rather than claiming
    /// sample-exact seeking. The value is generous enough to cover the largest
    /// common packet sizes (e.g. AAC 1024, MP3 1152) plus codec priming.
    pub const SEEK_COARSE_TOLERANCE_FRAMES: u64 = 4_096;

    pub fn staging_buffer_bytes(&self) -> usize {
        self.sample_buf
            .as_ref()
            .map_or(0, |buffer| buffer.capacity() * std::mem::size_of::<f64>())
    }

    fn decode_next_span(&mut self) -> Result<Option<(usize, usize)>, DecoderError> {
        if self.finished {
            return Ok(None);
        }
        if self
            .cancel_token
            .as_ref()
            .is_some_and(DecodeCancelToken::is_cancelled)
        {
            return Err(DecoderError::Canceled);
        }

        loop {
            if self
                .cancel_token
                .as_ref()
                .is_some_and(DecodeCancelToken::is_cancelled)
            {
                return Err(DecoderError::Canceled);
            }
            let packet = match self.format_reader.next_packet() {
                Ok(p) => p,
                Err(symphonia::core::errors::Error::IoError(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    self.finished = true;
                    return Ok(None);
                }
                Err(symphonia::core::errors::Error::IoError(e))
                    if e.kind() == std::io::ErrorKind::Interrupted =>
                {
                    return Err(DecoderError::Canceled);
                }
                Err(e) => return Err(DecoderError::Decoder(e.to_string())),
            };

            if packet.track_id() != self.track_id {
                continue;
            }

            let decoded = match self.decoder.decode(&packet) {
                Ok(d) => d,
                Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
                Err(e) => return Err(DecoderError::Decoder(e.to_string())),
            };

            let duration = decoded.capacity();
            let Some(sample_buf) = self.sample_buf.as_mut() else {
                return Err(DecoderError::Decoder(
                    "Failed to allocate decoder sample buffer".to_string(),
                ));
            };
            let required_samples = duration.saturating_mul(decoded.spec().channels.count());
            if required_samples > sample_buf.capacity() {
                return Err(DecoderError::Decoder(format!(
                    "decoded packet exceeds fixed staging capacity: required {} samples, reserved {}",
                    required_samples,
                    sample_buf.capacity()
                )));
            }
            sample_buf.copy_interleaved_ref(decoded);

            let samples = sample_buf.samples();
            let channels = self.info.channels;
            let mut start = 0;
            let mut end = samples.len();

            // Encoder-delay trimming applies ONLY at the true start of the
            // stream. `at_stream_start` is cleared once the leading delay is
            // fully consumed and is never re-armed by `seek()`, so a seek to a
            // non-zero position does not re-trim `encoder_delay` from the
            // post-seek stream.
            if self.at_stream_start {
                let delay_frames = self.info.encoder_delay as u64;
                let delay_samples = delay_frames * channels as u64;
                if self.samples_output < delay_samples {
                    let skip = (delay_samples - self.samples_output).min(end as u64) as usize;
                    start += skip;
                    self.samples_output += skip as u64;
                    if start == end {
                        continue;
                    }
                }
                // Either there was no delay or it has now been fully skipped;
                // real audio samples are about to be emitted, so we have left
                // the start-of-stream region.
                self.at_stream_start = false;
            }

            let total_frames = self.info.total_frames.unwrap_or(u64::MAX);
            let padding_frames = self.info.end_padding as u64;
            let effective_total = total_frames.saturating_sub(padding_frames);
            let current_frame = self.samples_output / channels as u64;
            let frames_in_chunk = (end - start) / channels;

            if current_frame + frames_in_chunk as u64 > effective_total {
                let frames_to_keep = effective_total.saturating_sub(current_frame) as usize;
                if frames_to_keep == 0 {
                    self.finished = true;
                    return Ok(None);
                }
                end = start + frames_to_keep * channels;
            }

            let appended = end - start;
            self.samples_output += appended as u64;
            return Ok(Some((start, end)));
        }
    }

    /// Decode the next packet and borrow the decoder-owned interleaved output.
    ///
    /// The returned slice remains valid until the next mutable decoder call.
    /// Callers that immediately copy into final storage avoid an intermediate
    /// caller-owned staging allocation.
    pub fn decode_next_borrowed(&mut self) -> Result<Option<&[f64]>, DecoderError> {
        let Some((start, end)) = self.decode_next_span()? else {
            return Ok(None);
        };
        let sample_buf = self.sample_buf.as_ref().ok_or_else(|| {
            DecoderError::Decoder("Decoded packet did not retain sample storage".to_string())
        })?;
        Ok(Some(&sample_buf.samples()[start..end]))
    }

    pub fn decode_next_into(&mut self, out: &mut Vec<f64>) -> Result<Option<usize>, DecoderError> {
        let Some(samples) = self.decode_next_borrowed()? else {
            return Ok(None);
        };
        let appended = samples.len();
        out.extend_from_slice(samples);
        Ok(Some(appended))
    }

    pub fn decode_next(&mut self) -> Result<Option<Vec<f64>>, DecoderError> {
        let mut samples = Vec::new();
        match self.decode_next_into(&mut samples)? {
            Some(_) => Ok(Some(samples)),
            None => Ok(None),
        }
    }

    pub fn decode_all(&mut self) -> Result<Vec<f64>, DecoderError> {
        let (max_memory_mb, max_memory_bytes) = configured_decode_memory_limit();

        let initial_capacity = if let Some(total_frames) = self.info.total_frames {
            let estimated_bytes = total_frames as usize * self.info.channels * F64_SAMPLE_BYTES;
            if estimated_bytes > max_memory_bytes {
                let estimated_mb = bytes_to_mib(estimated_bytes);
                return Err(DecoderError::Decoder(format!(
                    "File too large to decode into memory: estimated {} MB (limit: {} MB). \
                     Use streaming mode instead or increase DECODE_MAX_MEMORY_MB env var.",
                    estimated_mb, max_memory_mb
                )));
            }

            let total_samples = total_frames as usize * self.info.channels;
            log::info!(
                "Pre-allocating buffer for {} samples (~{} MB)",
                total_samples,
                bytes_to_mib(total_samples * F64_SAMPLE_BYTES)
            );
            total_samples
        } else {
            0
        };

        let mut all_samples = Vec::with_capacity(initial_capacity);
        while self.decode_next_into(&mut all_samples)?.is_some() {
            let current_bytes = all_samples.len() * F64_SAMPLE_BYTES;
            if current_bytes > max_memory_bytes {
                let current_mb = bytes_to_mib(current_bytes);
                return Err(DecoderError::Decoder(format!(
                    "Memory limit exceeded during decode: {} MB (limit: {} MB). \
                     File may be corrupted or extremely long.",
                    current_mb, max_memory_mb
                )));
            }
        }

        let delay_trimmed = self.info.encoder_delay;
        let padding_trimmed = self.info.end_padding;

        if delay_trimmed > 0 || padding_trimmed > 0 {
            log::info!(
                "Decoded {} samples (trimmed {} delay + {} padding for gapless)",
                all_samples.len(),
                delay_trimmed,
                padding_trimmed
            );
        } else {
            log::info!("Decoded {} total samples (f64)", all_samples.len());
        }

        Ok(all_samples)
    }

    /// Seek to `time_secs` using Symphonia's coarse seek mode.
    ///
    /// Seeking is **Coarse only** (`SeekMode::Coarse`): the decoder lands on a
    /// packet boundary at or before the requested time, so the realized
    /// position has bounded inaccuracy (see [`Self::SEEK_COARSE_TOLERANCE_FRAMES`]).
    /// A sample-exact (`Accurate`) mode is intentionally not offered.
    ///
    /// `samples_output` is reset so end-padding accounting tracks the new
    /// position, but encoder-delay trimming is **not** re-armed: the leading
    /// `encoder_delay` is only trimmed at the true start of the stream, never
    /// after a seek to a non-zero position.
    pub fn seek(&mut self, time_secs: f64) -> Result<(), DecoderError> {
        use symphonia::core::formats::SeekTo;
        use symphonia::core::units::Time;

        let seek_to = SeekTo::Time {
            time: Time::from(time_secs),
            track_id: Some(self.track_id),
        };

        let seeked_to = self
            .format_reader
            .seek(symphonia::core::formats::SeekMode::Coarse, seek_to)
            .map_err(map_seek_error)?;

        self.decoder.reset();
        self.finished = false;
        // Track the realized seek position (in frames) so end-padding trimming
        // stays correct relative to the stream. Crucially, `at_stream_start`
        // is NOT reset to true here, so the post-seek stream is not re-trimmed
        // for encoder delay.
        self.samples_output = seeked_to
            .actual_ts
            .saturating_mul(self.info.channels as u64);
        self.at_stream_start = false;

        Ok(())
    }

    /// Realized first-decoded-frame position after the most recent seek, in
    /// frames. Returns `samples_output / channels`; immediately after a
    /// successful `seek()` this reflects the coarse seek target.
    pub fn current_frame(&self) -> u64 {
        let channels = self.info.channels.max(1) as u64;
        self.samples_output / channels
    }
}

/// Map a Symphonia probe failure to a typed [`DecoderError`].
///
/// Genuinely unrecognized / unsupported container input surfaces as the typed
/// [`DecoderError::UnsupportedFormat`] rather than a stringly generic error.
/// Symphonia signals "no registered format matched the input" as either
/// `Error::Unsupported` or, for short/garbage byte streams that exhaust the
/// probe window, an `Error::IoError(UnexpectedEof)`; both mean "this is not a
/// container we can decode", so both map to `UnsupportedFormat`. Any other
/// failure keeps its description under [`DecoderError::Probe`] (documented
/// reason string).
fn map_probe_error(e: symphonia::core::errors::Error) -> DecoderError {
    use symphonia::core::errors::Error;
    match e {
        Error::Unsupported(_) => DecoderError::UnsupportedFormat,
        Error::IoError(io) if io.kind() == std::io::ErrorKind::UnexpectedEof => {
            DecoderError::UnsupportedFormat
        }
        other => DecoderError::Probe(other.to_string()),
    }
}

/// Derive a positional [`ChannelLayout`] from the container's channel mask.
///
/// Symphonia reports channels in ascending channel-mask bit order, which is the
/// same order it interleaves decoded samples, so we walk the known positions in
/// that order. If the mask contains channels we do not classify (e.g. height
/// channels) the derived count would be shorter than the actual interleave, so
/// we fall back to a count-based layout to stay consistent with the buffer.
fn layout_from_codec(channels: Option<Channels>, count: usize) -> ChannelLayout {
    let Some(channels) = channels else {
        return ChannelLayout::from_count(count);
    };

    // Ascending channel-mask bit order == Symphonia's interleave order.
    let ordered = [
        (Channels::FRONT_LEFT, ChannelPosition::FrontLeft),
        (Channels::FRONT_RIGHT, ChannelPosition::FrontRight),
        (Channels::FRONT_CENTRE, ChannelPosition::FrontCenter),
        (Channels::LFE1, ChannelPosition::LowFrequency),
        (Channels::REAR_LEFT, ChannelPosition::RearLeft),
        (Channels::REAR_RIGHT, ChannelPosition::RearRight),
        (
            Channels::FRONT_LEFT_CENTRE,
            ChannelPosition::FrontLeftCenter,
        ),
        (
            Channels::FRONT_RIGHT_CENTRE,
            ChannelPosition::FrontRightCenter,
        ),
        (Channels::REAR_CENTRE, ChannelPosition::RearCenter),
        (Channels::SIDE_LEFT, ChannelPosition::SideLeft),
        (Channels::SIDE_RIGHT, ChannelPosition::SideRight),
    ];

    let mut positions = Vec::with_capacity(count);
    for (flag, position) in ordered {
        if channels.contains(flag) {
            positions.push(position);
        }
    }

    if positions.len() == count {
        ChannelLayout::from_positions(positions)
    } else {
        ChannelLayout::from_count(count)
    }
}

/// Map a Symphonia seek failure to a typed [`DecoderError`].
///
/// Unsupported/unseekable streams surface as [`DecoderError::UnsupportedFormat`]
/// rather than a stringly-typed generic error; other failures keep their
/// description under [`DecoderError::Decoder`].
fn map_seek_error(e: symphonia::core::errors::Error) -> DecoderError {
    use symphonia::core::errors::Error;
    match e {
        Error::Unsupported(_) => DecoderError::UnsupportedFormat,
        other => DecoderError::Decoder(other.to_string()),
    }
}
