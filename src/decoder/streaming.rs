use std::path::Path;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::formats::FormatOptions;
use symphonia::core::meta::MetadataOptions;

use super::error::{DecodeCancelToken, DecoderError};
use super::metadata::{extract_metadata, merge_metadata_revision, AudioInfo};
use super::source::{
    bytes_to_mib, configured_decode_memory_limit, open_media_source, HttpCredentials,
    F64_SAMPLE_BYTES,
};

/// Streaming audio decoder using Symphonia.
pub struct StreamingDecoder {
    format_reader: Box<dyn symphonia::core::formats::FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    track_id: u32,
    pub info: AudioInfo,
    sample_buf: Option<SampleBuffer<f64>>,
    samples_output: u64,
    finished: bool,
    cancel_token: Option<DecodeCancelToken>,
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
        let (mss, hint) = open_media_source(path.as_ref(), credentials, cancel_token.clone())?;

        let format_opts = FormatOptions::default();
        let metadata_opts = MetadataOptions::default();
        let mut probed = symphonia::default::get_probe()
            .format(&hint, mss, &format_opts, &metadata_opts)
            .map_err(|e| DecoderError::Probe(e.to_string()))?;

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
            bits_per_sample,
            total_frames,
            duration_secs,
            encoder_delay,
            end_padding,
            metadata,
        };

        let decoder_opts = DecoderOptions::default();
        let decoder = symphonia::default::get_codecs()
            .make(codec_params, &decoder_opts)
            .map_err(|e| DecoderError::Decoder(e.to_string()))?;

        log::info!(
            "Opened audio source: {} Hz, {} ch, {:?}s",
            sample_rate,
            channels,
            duration_secs
        );

        Ok(Self {
            format_reader,
            decoder,
            track_id,
            info,
            sample_buf: None,
            samples_output: 0,
            finished: false,
            cancel_token,
        })
    }

    pub fn decode_next_into(&mut self, out: &mut Vec<f64>) -> Result<Option<usize>, DecoderError> {
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

            let spec = *decoded.spec();
            let duration = decoded.capacity();

            if self
                .sample_buf
                .as_ref()
                .is_none_or(|buffer| buffer.capacity() < duration)
            {
                self.sample_buf = Some(SampleBuffer::new(duration as u64, spec));
            }

            let Some(sample_buf) = self.sample_buf.as_mut() else {
                return Err(DecoderError::Decoder(
                    "Failed to allocate decoder sample buffer".to_string(),
                ));
            };
            sample_buf.copy_interleaved_ref(decoded);

            let samples = sample_buf.samples();
            let channels = self.info.channels;
            let mut start = 0;
            let mut end = samples.len();

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
            out.extend_from_slice(&samples[start..end]);
            self.samples_output += appended as u64;
            return Ok(Some(appended));
        }
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

    pub fn seek(&mut self, time_secs: f64) -> Result<(), DecoderError> {
        use symphonia::core::formats::SeekTo;
        use symphonia::core::units::Time;

        let seek_to = SeekTo::Time {
            time: Time::from(time_secs),
            track_id: Some(self.track_id),
        };

        self.format_reader
            .seek(symphonia::core::formats::SeekMode::Coarse, seek_to)
            .map_err(|e| DecoderError::Decoder(e.to_string()))?;

        self.decoder.reset();
        self.finished = false;
        self.samples_output = 0;

        Ok(())
    }
}
