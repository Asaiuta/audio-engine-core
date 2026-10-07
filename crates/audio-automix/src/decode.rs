//! Media-source adaptation and bounded head/tail decoding.
use crate::descriptors::{
    AutomixDescriptorAnalysis, DescriptorReport, DescriptorSegmentAccumulator,
    DescriptorSegmentRole,
};
use crate::features::{envelope_window_size, AnalysisSegment, SegmentAnalyzer};
use crate::placement::finalize_analysis;
use crate::{
    check_cancel, AutomixAnalysis, AutomixAnalysisMode, AutomixAnalysisOptions, AutomixError,
    MAX_DECLARED_DURATION_SEC,
};
use audio_engine_core::analysis::LoudnessMeter;
use audio_engine_core::decoder::{
    AudioInfo, DecodeCancelToken, DecoderError, HttpCredentials, MediaLocation, StreamingDecoder,
};
use std::ops::Range;

// The same bounded driver serves the public decoder and deterministic packet /
// metadata tests. This is private; it does not add a second media abstraction.
pub(super) trait AnalysisDecoder {
    fn info(&self) -> &AudioInfo;
    fn decode_next_into(&mut self, buffer: &mut Vec<f64>) -> Result<Option<usize>, DecoderError>;
    fn seek(&mut self, seconds: f64) -> Result<(), DecoderError>;
    fn current_frame(&self) -> u64;
}

impl AnalysisDecoder for StreamingDecoder {
    fn info(&self) -> &AudioInfo {
        self.info()
    }

    fn decode_next_into(&mut self, buffer: &mut Vec<f64>) -> Result<Option<usize>, DecoderError> {
        self.decode_next_into(buffer)
    }

    fn seek(&mut self, seconds: f64) -> Result<(), DecoderError> {
        self.seek(seconds)
    }

    fn current_frame(&self) -> u64 {
        self.current_frame()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FrameWindow {
    pub(super) start: u64,
    pub(super) end: u64,
}

impl FrameWindow {
    pub(super) fn len(self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    pub(super) fn start_time(self, sample_rate: u32) -> f64 {
        self.start as f64 / sample_rate.max(1) as f64
    }

    /// Whether this window's last planned frame is the known track end.
    pub(super) fn ends_at_track_end(self, track_frames: Option<u64>) -> bool {
        track_frames.is_some_and(|frames| self.end >= frames)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AnalysisWindowPlan {
    pub(super) head: FrameWindow,
    pub(super) tail: Option<FrameWindow>,
}

impl AnalysisWindowPlan {
    pub(super) fn new(
        mode: AutomixAnalysisMode,
        track_frames: Option<u64>,
        window_frames: u64,
    ) -> Self {
        let window_frames = window_frames.max(1);
        let head_end = track_frames.map_or(window_frames, |frames| frames.min(window_frames));
        let head = FrameWindow {
            start: 0,
            end: head_end,
        };
        let tail = track_frames.and_then(|frames| {
            (mode.includes_tail() && frames > head.end).then(|| FrameWindow {
                start: head.end.max(frames.saturating_sub(window_frames)),
                end: frames,
            })
        });

        Self { head, tail }
    }
}

/// Run bounded offline AutoMix analysis on a media location.
pub fn analyze_automix(
    location: MediaLocation,
    credentials: Option<HttpCredentials>,
    options: AutomixAnalysisOptions,
) -> Result<AutomixAnalysis, AutomixError> {
    analyze_automix_with_cancel(location, credentials, options, None)
}

/// Run bounded AutoMix analysis with a cooperative cancel token.
pub fn analyze_automix_with_cancel(
    location: MediaLocation,
    credentials: Option<HttpCredentials>,
    options: AutomixAnalysisOptions,
    cancel_token: Option<DecodeCancelToken>,
) -> Result<AutomixAnalysis, AutomixError> {
    let mut decoder = open_decoder(location, credentials.as_ref(), cancel_token.clone())?;
    let canceled = || {
        cancel_token
            .as_ref()
            .is_some_and(DecodeCancelToken::is_cancelled)
    };
    analyze_decoder(&mut decoder, options, Some(&canceled), None)
}

/// Analyze the same bounded source windows and return an opt-in L1 report.
///
/// The companion has its own schema and uses native-rate f64 arithmetic-mean
/// mono, complete 4096-sample periodic Hann windows and a 1024-sample hop.
/// It records actual source-frame coverage, signal measurements and per-frame
/// centroid/flatness means with valid denominators. Head and tail stay separate.
/// These measurements do not influence the existing analysis or cut decisions.
///
/// Run on a worker. This explicitly adds a descriptor FFT; the original entry
/// points create no descriptor state. Any failure or cancellation returns an
/// error for the whole request. A pre-canceled token is checked before opening
/// the source. This report does not imply whole-track or musical quality.
pub fn analyze_automix_with_descriptors(
    location: MediaLocation,
    credentials: Option<HttpCredentials>,
    options: AutomixAnalysisOptions,
    cancel_token: Option<DecodeCancelToken>,
) -> Result<AutomixDescriptorAnalysis, AutomixError> {
    let mut decoder = open_decoder(location, credentials.as_ref(), cancel_token.clone())?;
    let canceled = || {
        cancel_token
            .as_ref()
            .is_some_and(DecodeCancelToken::is_cancelled)
    };
    let mut descriptors =
        DescriptorReport::new(decoder.info().sample_rate, decoder.info().channels.max(1));
    let analysis = analyze_decoder(
        &mut decoder,
        options,
        Some(&canceled),
        Some(&mut descriptors),
    )?;
    Ok(AutomixDescriptorAnalysis {
        analysis,
        descriptors,
    })
}

fn open_decoder(
    location: MediaLocation,
    credentials: Option<&HttpCredentials>,
    cancel_token: Option<DecodeCancelToken>,
) -> Result<StreamingDecoder, AutomixError> {
    if cancel_token
        .as_ref()
        .is_some_and(DecodeCancelToken::is_cancelled)
    {
        return Err(AutomixError::Canceled);
    }
    Ok(StreamingDecoder::open_with_credentials_and_cancel(
        location,
        credentials,
        cancel_token,
    )?)
}

pub(super) fn analyze_decoder<D: AnalysisDecoder>(
    decoder: &mut D,
    options: AutomixAnalysisOptions,
    cancel: Option<&dyn Fn() -> bool>,
    mut descriptors: Option<&mut DescriptorReport>,
) -> Result<AutomixAnalysis, AutomixError> {
    let options = options.normalized();
    let sample_rate = decoder.info().sample_rate;
    let channels = decoder.info().channels.max(1);
    let declared_duration = decoder.info().duration_secs.filter(is_plausible_duration);
    let track_frames = decoder
        .info()
        .total_frames
        .filter(|frames| is_plausible_duration(&frames_to_seconds(*frames, sample_rate)))
        .or_else(|| {
            declared_duration.and_then(|duration| frames_for_duration(duration, sample_rate))
        });
    let duration = declared_duration
        .or_else(|| track_frames.map(|frames| frames_to_seconds(frames, sample_rate)))
        .unwrap_or(0.0);
    let window_frames = frames_for_duration(options.max_analyze_time_sec, sample_rate)
        .unwrap_or(1)
        .max(1);
    let plan = AnalysisWindowPlan::new(options.mode, track_frames, window_frames);
    let mut meter = LoudnessMeter::new(channels, sample_rate)?;
    let mut head = AnalysisSegment::at(plan.head.start_time(sample_rate));
    let mut tail = AnalysisSegment::default();
    let mut head_descriptors = descriptors
        .as_ref()
        .map(|_| DescriptorSegmentAccumulator::new(sample_rate, channels))
        .transpose()?;

    decode_segment(
        decoder,
        &mut meter,
        &mut head,
        0,
        plan.head.len(),
        cancel,
        head_descriptors.as_mut(),
    )?;
    head.reaches_input_end = head.reaches_input_end || plan.head.ends_at_track_end(track_frames);
    if let (Some(report), Some(accumulator)) = (descriptors.as_deref_mut(), head_descriptors) {
        report.segments.push(accumulator.finish(
            DescriptorSegmentRole::Head,
            plan.head.start,
            plan.head.end,
        ));
    }

    if let Some(tail_window) = plan.tail {
        check_cancel(cancel)?;
        decoder.seek(tail_window.start_time(sample_rate))?;
        let realized_start = decoder.current_frame();
        let skip_frames = tail_window.start.checked_sub(realized_start).ok_or(
            AutomixError::TailSeekPastStart {
                planned_frame: tail_window.start,
                realized_frame: realized_start,
            },
        )?;
        tail = AnalysisSegment::at(tail_window.start_time(sample_rate));
        let mut tail_descriptors = descriptors
            .as_ref()
            .map(|_| DescriptorSegmentAccumulator::new(sample_rate, channels))
            .transpose()?;
        decode_segment(
            decoder,
            &mut meter,
            &mut tail,
            skip_frames,
            tail_window.len(),
            cancel,
            tail_descriptors.as_mut(),
        )?;
        tail.reaches_input_end =
            tail.reaches_input_end || tail_window.ends_at_track_end(track_frames);
        if let (Some(report), Some(accumulator)) = (descriptors, tail_descriptors) {
            report.segments.push(accumulator.finish(
                DescriptorSegmentRole::Tail,
                tail_window.start,
                tail_window.end,
            ));
        }
    }

    finalize_analysis(
        &options,
        duration,
        sample_rate,
        &meter,
        &head,
        &tail,
        cancel,
    )
}

pub(super) fn decode_segment<D: AnalysisDecoder>(
    decoder: &mut D,
    meter: &mut LoudnessMeter,
    segment: &mut AnalysisSegment,
    skip_frames: u64,
    take_frames: u64,
    cancel_token: Option<&dyn Fn() -> bool>,
    mut descriptors: Option<&mut DescriptorSegmentAccumulator>,
) -> Result<(), AutomixError> {
    let sample_rate = decoder.info().sample_rate;
    let channels = decoder.info().channels.max(1);
    let window_size = envelope_window_size(sample_rate);
    let mut chunk = Vec::with_capacity(window_size * channels);
    let mut analyzer = SegmentAnalyzer::new(sample_rate, channels);
    let mut skip_remaining = skip_frames;
    let mut take_remaining = take_frames;

    while take_remaining > 0 {
        check_cancel(cancel_token)?;
        chunk.clear();
        let Some(sample_count) = decoder.decode_next_into(&mut chunk)? else {
            // End of stream before the window filled: its last selected frame
            // is the end of the input.
            segment.reaches_input_end = true;
            break;
        };
        if sample_count == 0 {
            continue;
        }
        let packet_frames = chunk.len() / channels;
        let Some(frame_range) =
            select_packet_frames(packet_frames, &mut skip_remaining, take_remaining)
        else {
            continue;
        };
        let sample_range = frame_range.start * channels..frame_range.end * channels;
        let selected_frames = (frame_range.end - frame_range.start) as u64;
        let selected = &chunk[sample_range];
        analyzer.process(selected, meter, segment, cancel_token)?;
        if let Some(descriptors) = descriptors.as_deref_mut() {
            descriptors.process(selected, cancel_token)?;
        }
        take_remaining -= selected_frames;
    }

    Ok(())
}

pub(super) fn select_packet_frames(
    packet_frames: usize,
    skip_remaining: &mut u64,
    take_remaining: u64,
) -> Option<Range<usize>> {
    let skipped = (*skip_remaining).min(packet_frames as u64) as usize;
    *skip_remaining -= skipped as u64;
    let available = packet_frames - skipped;
    let selected = take_remaining.min(available as u64) as usize;
    (selected > 0).then_some(skipped..skipped + selected)
}

pub(super) fn frames_for_duration(duration: f64, sample_rate: u32) -> Option<u64> {
    if !duration.is_finite() || duration < 0.0 || sample_rate == 0 {
        return None;
    }
    let frames = duration * sample_rate as f64;
    (frames.is_finite() && frames <= u64::MAX as f64).then(|| frames.ceil() as u64)
}

fn frames_to_seconds(frames: u64, sample_rate: u32) -> f64 {
    frames as f64 / sample_rate.max(1) as f64
}

/// Whether a container-declared track duration may be used as the analysis
/// timeline.
///
/// An implausible value is discarded rather than clamped: clamping would report
/// a confident timeline the file never supported, whereas discarding falls back
/// to the duration actually measured from decoded head evidence.
pub(super) fn is_plausible_duration(duration: &f64) -> bool {
    duration.is_finite() && *duration > 0.0 && *duration <= MAX_DECLARED_DURATION_SEC
}

#[cfg(test)]
mod tests;
