use symphonia::core::audio::Channels;
use symphonia::core::codecs::audio::well_known::{CODEC_ID_MP3, CODEC_ID_VORBIS};
use symphonia::core::codecs::audio::{AudioCodecId, AudioDecoder, AudioDecoderOptions};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, TimeBase, Timestamp};

use super::channel_layout::layout_from_codec;
use super::error::{DecodeCancelToken, DecoderError};
use super::metadata::{extract_metadata, AudioInfo};
use super::source::{
    bytes_to_mib, configured_decode_memory_limit, HttpCredentials, MediaLocation, OpenedMediaSource,
};

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
///
/// Gapless trimming has exactly one owner per codec. Symphonia owns MP3 and
/// Vorbis packet trim/reset behavior; codecs that do not consume its gapless
/// option retain the crate's Track-level delay/padding fallback.
pub struct StreamingDecoder {
    format_reader: Box<dyn FormatReader + 'static>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    track_time_base: Option<TimeBase>,
    track_start_ts: Timestamp,
    /// Observed track metadata. Private because the decoder also trusts these
    /// fields for staging geometry, gapless counters, allocation, seek math,
    /// and position; see [`StreamingDecoder::info`].
    info: AudioInfo,
    sample_buf: Option<Vec<f64>>,
    raw_total_frames: Option<u64>,
    gapless_owner: GaplessOwner,
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
const DECODED_SAMPLE_BYTES: usize = std::mem::size_of::<f64>();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DecodedBufferSizePlan {
    samples: usize,
    bytes: usize,
}

impl DecodedBufferSizePlan {
    fn from_frames(frames: u64, channels: usize) -> Result<Self, DecoderError> {
        let frames = usize::try_from(frames).map_err(|_| decoded_buffer_size_overflow())?;
        let samples = frames
            .checked_mul(channels)
            .ok_or_else(decoded_buffer_size_overflow)?;
        Self::from_samples(samples)
    }

    fn from_samples(samples: usize) -> Result<Self, DecoderError> {
        let bytes = samples
            .checked_mul(DECODED_SAMPLE_BYTES)
            .ok_or_else(decoded_buffer_size_overflow)?;
        Ok(Self { samples, bytes })
    }

    fn after_append(current_samples: usize, incoming_samples: usize) -> Result<Self, DecoderError> {
        let samples = current_samples
            .checked_add(incoming_samples)
            .ok_or_else(decoded_buffer_size_overflow)?;
        Self::from_samples(samples)
    }
}

fn decoded_buffer_size_overflow() -> DecoderError {
    DecoderError::Decoder("decoded buffer size overflow".to_string())
}

fn reserve_decoded_samples(samples: &mut Vec<f64>, additional: usize) -> Result<(), DecoderError> {
    samples.try_reserve_exact(additional).map_err(|error| {
        DecoderError::Decoder(format!(
            "failed to reserve decoded buffer capacity: {error}"
        ))
    })
}

fn append_decoded_samples_within_budget(
    destination: &mut Vec<f64>,
    incoming: &[f64],
    max_memory_mb: usize,
    max_memory_bytes: usize,
) -> Result<(), DecoderError> {
    let next_size = DecodedBufferSizePlan::after_append(destination.len(), incoming.len())?;
    if next_size.bytes > max_memory_bytes {
        return Err(DecoderError::Decoder(format!(
            "Memory limit exceeded during decode: {} MB (limit: {} MB). \
             File may be corrupted or extremely long.",
            bytes_to_mib(next_size.bytes),
            max_memory_mb
        )));
    }

    reserve_decoded_samples(destination, incoming.len())?;
    destination.extend_from_slice(incoming);
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GaplessOwner {
    NativeDecoder,
    TrackFallback,
}

impl GaplessOwner {
    fn for_codec(codec: AudioCodecId) -> Self {
        // Symphonia 0.6 currently reads AudioDecoderOptions::gapless only in
        // the MPEG Layer III and Vorbis decoders. Keep this as an allowlist so
        // an upstream codec that ignores the option cannot silently disable
        // the Track-level fallback.
        if matches!(codec, CODEC_ID_MP3 | CODEC_ID_VORBIS) {
            Self::NativeDecoder
        } else {
            Self::TrackFallback
        }
    }

    fn uses_native_decoder(self) -> bool {
        self == Self::NativeDecoder
    }

    fn uses_track_fallback(self) -> bool {
        self == Self::TrackFallback
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::NativeDecoder => "native-decoder",
            Self::TrackFallback => "track-fallback",
        }
    }
}

/// Probed streaming source awaiting fixed decoder-staging allocation.
pub struct StreamingDecoderBuilder {
    format_reader: Box<dyn FormatReader + 'static>,
    track_id: u32,
    track_time_base: Option<TimeBase>,
    track_start_ts: Timestamp,
    /// Observed track metadata. Private for the same reason as
    /// [`StreamingDecoder::info`]; read it through [`Self::info`].
    info: AudioInfo,
    raw_total_frames: Option<u64>,
    gapless_owner: GaplessOwner,
    staging_frames: u64,
    cancel_token: Option<DecodeCancelToken>,
}

impl StreamingDecoderBuilder {
    /// Observed track metadata for the probed source.
    pub fn info(&self) -> &AudioInfo {
        &self.info
    }

    /// Exact fixed interleaved `f64` staging payload allocated by [`Self::build`].
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
        let codec_params = track
            .codec_params
            .as_ref()
            .and_then(CodecParameters::audio)
            .ok_or(DecoderError::NoAudioTrack)?;
        let decoder_options =
            AudioDecoderOptions::default().gapless(self.gapless_owner.uses_native_decoder());
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(codec_params, &decoder_options)
            .map_err(|error| DecoderError::Decoder(error.to_string()))?;
        let staging_samples = usize::try_from(self.staging_frames)
            .ok()
            .and_then(|frames| frames.checked_mul(self.info.channels))
            .ok_or_else(|| DecoderError::Decoder("decoder staging size overflow".to_string()))?;
        let sample_buf = vec![0.0; staging_samples];

        Ok(StreamingDecoder {
            format_reader: self.format_reader,
            decoder,
            track_id: self.track_id,
            track_time_base: self.track_time_base,
            track_start_ts: self.track_start_ts,
            info: self.info,
            sample_buf: Some(sample_buf),
            raw_total_frames: self.raw_total_frames,
            gapless_owner: self.gapless_owner,
            samples_output: 0,
            finished: false,
            at_stream_start: self.gapless_owner.uses_track_fallback(),
            cancel_token: self.cancel_token,
        })
    }
}

impl StreamingDecoder {
    /// Observed track metadata: format geometry, duration, gapless counters,
    /// and tags.
    ///
    /// Read-only by design. The decoder trusts these same fields for staging
    /// geometry, gapless trimming, buffer sizing, seek arithmetic, and reported
    /// position, so a caller-supplied edit would be an unvalidated control
    /// channel into decode state rather than an observation.
    pub fn info(&self) -> &AudioInfo {
        &self.info
    }

    /// Inject synthetic gapless counters for tests.
    ///
    /// WAV fixtures carry no encoder delay or end padding, so the trimming
    /// tests have to supply them. This exists instead of a public mutable
    /// `info` field: it names exactly the two fields a test may override,
    /// keeps every other field observation-only, and cannot be reached from
    /// outside the crate.
    #[cfg(test)]
    pub(crate) fn set_gapless_counters_for_test(&mut self, encoder_delay: u32, end_padding: u32) {
        self.info.encoder_delay = encoder_delay;
        self.info.end_padding = end_padding;
    }

    pub fn open(location: MediaLocation) -> Result<Self, DecoderError> {
        Self::open_with_credentials(location, None)
    }

    pub fn open_with_credentials(
        location: MediaLocation,
        credentials: Option<&HttpCredentials>,
    ) -> Result<Self, DecoderError> {
        Self::open_with_credentials_and_cancel(location, credentials, None)
    }

    pub fn open_with_credentials_and_cancel(
        location: MediaLocation,
        credentials: Option<&HttpCredentials>,
        cancel_token: Option<DecodeCancelToken>,
    ) -> Result<Self, DecoderError> {
        let source = OpenedMediaSource::open_with_credentials_and_cancel(
            location,
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
        let mut format_reader = symphonia::default::get_probe()
            .probe(&hint, stream, format_opts, metadata_opts)
            .map_err(map_probe_error)?;

        let metadata = extract_metadata(&mut *format_reader);

        let track = format_reader
            .default_track(TrackType::Audio)
            .ok_or(DecoderError::NoAudioTrack)?;

        let track_id = track.id;
        let track_time_base = track.time_base;
        let track_start_ts = track.start_ts;
        let codec_params = track
            .codec_params
            .as_ref()
            .and_then(CodecParameters::audio)
            .ok_or(DecoderError::NoAudioTrack)?;
        let gapless_owner = GaplessOwner::for_codec(codec_params.codec);
        let sample_rate = codec_params.sample_rate.unwrap_or(44100);
        let channels = codec_params
            .channels
            .as_ref()
            .map(Channels::count)
            .unwrap_or(2);
        let channel_layout = layout_from_codec(codec_params.channels.as_ref(), channels);
        let bits_per_sample = codec_params.bits_per_sample;
        let total_frames = track.num_frames;
        let duration_secs = track
            .duration
            .and_then(|duration| {
                track.time_base.and_then(|time_base| {
                    Timestamp::try_from(duration.get())
                        .ok()
                        .and_then(|timestamp| time_base.calc_time(timestamp))
                })
            })
            .map(|time| time.as_secs_f64())
            .or_else(|| total_frames.map(|frames| frames as f64 / sample_rate as f64));
        let encoder_delay = track.delay.unwrap_or(0);
        let end_padding = track.padding.unwrap_or(0);
        let raw_total_frames = total_frames.map(|frames| {
            frames
                .saturating_add(u64::from(encoder_delay))
                .saturating_add(u64::from(end_padding))
        });

        if encoder_delay > 0 || end_padding > 0 {
            log::debug!(
                "Codec delay compensation: owner={}, delay={}, padding={} samples",
                gapless_owner.as_str(),
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
            track_time_base,
            track_start_ts,
            info,
            raw_total_frames,
            gapless_owner,
            staging_frames,
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
                Ok(Some(packet)) => packet,
                Ok(None) => {
                    self.finished = true;
                    return Ok(None);
                }
                Err(symphonia::core::errors::Error::ResetRequired) => {
                    return Err(DecoderError::Decoder(
                        "format reader reset required after track change".to_string(),
                    ));
                }
                Err(e) => return Err(DecoderError::Decoder(e.to_string())),
            };

            if packet.track_id != self.track_id {
                continue;
            }

            let decoded = match self.decoder.decode(&packet) {
                Ok(d) => d,
                Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
                Err(e) => return Err(DecoderError::Decoder(e.to_string())),
            };

            let duration = decoded.frames();
            let decoded_channels = decoded.num_planes();
            if decoded_channels != self.info.channels {
                return Err(DecoderError::Decoder(format!(
                    "decoded channel count changed from {} to {}",
                    self.info.channels, decoded_channels
                )));
            }
            let Some(sample_buf) = self.sample_buf.as_mut() else {
                return Err(DecoderError::Decoder(
                    "Failed to allocate decoder sample buffer".to_string(),
                ));
            };
            let required_samples = duration.saturating_mul(decoded_channels);
            if required_samples == 0 {
                // Stateful native gapless decoders may intentionally discard
                // a preroll/reset packet. Do not expose an empty Some(&[]) to
                // callers; continue until audio or EOF is available.
                continue;
            }
            if required_samples > sample_buf.capacity() {
                return Err(DecoderError::Decoder(format!(
                    "decoded packet exceeds fixed staging capacity: required {} samples, reserved {}",
                    required_samples,
                    sample_buf.capacity()
                )));
            }
            decoded.copy_to_slice_interleaved(&mut sample_buf[..required_samples]);

            let channels = self.info.channels;
            let mut start = 0;
            let mut end = required_samples;

            // Encoder-delay trimming applies ONLY at the true start of the
            // stream. `at_stream_start` is cleared once the leading delay is
            // fully consumed and is never re-armed by `seek()`, so a seek to a
            // non-zero position does not re-trim `encoder_delay` from the
            // post-seek stream.
            if self.gapless_owner.uses_track_fallback() && self.at_stream_start {
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

            if self.gapless_owner.uses_track_fallback() {
                let total_frames = self.raw_total_frames.unwrap_or(u64::MAX);
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
        Ok(Some(&sample_buf[start..end]))
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

        let initial_size = if let Some(total_frames) = self.raw_total_frames {
            let size = DecodedBufferSizePlan::from_frames(total_frames, self.info.channels)?;
            if size.bytes > max_memory_bytes {
                return Err(DecoderError::Decoder(format!(
                    "File too large to decode into memory: estimated {} MB (limit: {} MB). \
                     Use streaming mode instead or increase DECODE_MAX_MEMORY_MB env var.",
                    bytes_to_mib(size.bytes),
                    max_memory_mb
                )));
            }

            log::info!(
                "Pre-allocating buffer for {} samples (~{} MB)",
                size.samples,
                bytes_to_mib(size.bytes)
            );
            size
        } else {
            DecodedBufferSizePlan::from_samples(0)?
        };

        let mut all_samples = Vec::new();
        reserve_decoded_samples(&mut all_samples, initial_size.samples)?;
        while let Some(samples) = self.decode_next_borrowed()? {
            append_decoded_samples_within_budget(
                &mut all_samples,
                samples,
                max_memory_mb,
                max_memory_bytes,
            )?;
        }

        let delay_trimmed = self.info.encoder_delay;
        let padding_trimmed = self.info.end_padding;

        if delay_trimmed > 0 || padding_trimmed > 0 {
            log::info!(
                "Decoded {} samples (gapless owner={}, delay={}, padding={})",
                all_samples.len(),
                self.gapless_owner.as_str(),
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
    /// `samples_output` is reset so fallback end-padding accounting tracks the
    /// new position. Track-level encoder-delay trimming is **not** re-armed;
    /// native MP3/Vorbis decoders instead consume packet-local trim and reset
    /// preroll according to their codec state.
    pub fn seek(&mut self, time_secs: f64) -> Result<(), DecoderError> {
        use symphonia::core::formats::SeekTo;
        let time = Time::try_from_secs_f64(time_secs)
            .ok_or_else(|| DecoderError::Decoder("invalid seek time".to_string()))?;
        let seek_to = SeekTo::Time {
            time,
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
        let frame_offset = self
            .timestamp_to_frame_offset(seeked_to.actual_ts)
            .ok_or_else(|| {
                DecoderError::Decoder("seek timestamp overflows frame position".to_string())
            })?;
        self.samples_output = frame_offset.saturating_mul(self.info.channels as u64);
        self.at_stream_start = false;

        Ok(())
    }

    fn timestamp_to_frame_offset(&self, timestamp: Timestamp) -> Option<u64> {
        timestamp_to_frame_offset(
            timestamp,
            self.track_start_ts,
            self.track_time_base,
            self.info.sample_rate,
        )
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

fn timestamp_to_frame_offset(
    timestamp: Timestamp,
    start_ts: Timestamp,
    time_base: Option<TimeBase>,
    sample_rate: u32,
) -> Option<u64> {
    let relative_ts = timestamp.get().checked_sub(start_ts.get())?;
    if relative_ts <= 0 {
        return Some(0);
    }

    let Some(time_base) = time_base else {
        return u64::try_from(relative_ts).ok();
    };

    let frame_numerator = i128::from(relative_ts)
        .checked_mul(i128::from(time_base.numer.get()))?
        .checked_mul(i128::from(sample_rate))?;
    let frame_count = frame_numerator / i128::from(time_base.denom.get());
    u64::try_from(frame_count).ok()
}

#[cfg(test)]
mod streaming_tests {
    use super::{
        append_decoded_samples_within_budget, timestamp_to_frame_offset, DecodedBufferSizePlan,
        GaplessOwner,
    };
    use symphonia::core::codecs::audio::well_known::{
        CODEC_ID_AAC, CODEC_ID_ALAC, CODEC_ID_FLAC, CODEC_ID_MP3, CODEC_ID_PCM_S16LE,
        CODEC_ID_VORBIS,
    };
    use symphonia::core::units::{TimeBase, Timestamp};

    #[test]
    fn seek_timestamp_uses_track_timebase_and_start_offset() {
        let time_base = TimeBase::try_new(1, 1_000).expect("valid timebase");

        assert_eq!(
            timestamp_to_frame_offset(
                Timestamp::new(975),
                Timestamp::new(-25),
                Some(time_base),
                48_000,
            ),
            Some(48_000)
        );
    }

    #[test]
    fn decoded_buffer_size_plan_computes_exact_interleaved_geometry() {
        let plan = DecodedBufferSizePlan::from_frames(128, 6).expect("valid geometry");

        assert_eq!(plan.samples, 768);
        assert_eq!(plan.bytes, 6_144);
    }

    #[test]
    fn decoded_buffer_size_plan_rejects_frame_and_channel_overflow() {
        assert!(DecodedBufferSizePlan::from_frames(u64::MAX, 2).is_err());

        let overflowing_frames = (usize::MAX as u64 / 2) + 1;
        assert!(DecodedBufferSizePlan::from_frames(overflowing_frames, 2).is_err());
        assert!(DecodedBufferSizePlan::from_samples(usize::MAX).is_err());
    }

    #[test]
    fn decoded_buffer_budget_rejection_does_not_mutate_destination() {
        let mut destination = vec![0.25];
        let original = destination.clone();

        let error = append_decoded_samples_within_budget(
            &mut destination,
            &[0.5, 0.75],
            0,
            2 * std::mem::size_of::<f64>(),
        )
        .expect_err("three samples exceed the two-sample budget");

        assert!(error.to_string().contains("Memory limit exceeded"));
        assert_eq!(destination, original);
    }

    #[test]
    fn gapless_owner_is_native_only_for_decoders_that_consume_the_option() {
        assert_eq!(
            GaplessOwner::for_codec(CODEC_ID_MP3),
            GaplessOwner::NativeDecoder
        );
        assert_eq!(
            GaplessOwner::for_codec(CODEC_ID_VORBIS),
            GaplessOwner::NativeDecoder
        );
        assert_eq!(
            GaplessOwner::for_codec(CODEC_ID_AAC),
            GaplessOwner::TrackFallback
        );
        assert_eq!(
            GaplessOwner::for_codec(CODEC_ID_FLAC),
            GaplessOwner::TrackFallback
        );
        assert_eq!(
            GaplessOwner::for_codec(CODEC_ID_ALAC),
            GaplessOwner::TrackFallback
        );
        assert_eq!(
            GaplessOwner::for_codec(CODEC_ID_PCM_S16LE),
            GaplessOwner::TrackFallback
        );
    }
}
