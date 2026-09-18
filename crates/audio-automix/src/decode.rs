//! Media-source adaptation and bounded head/tail decoding.
use crate::features::{AnalysisSegment, SegmentAnalyzer};
use crate::placement::finalize_analysis;
use crate::{
    check_cancel, AutomixAnalysis, AutomixAnalysisMode, AutomixAnalysisOptions, AutomixError,
    MAX_DECLARED_DURATION_SEC, WINDOW_SIZE_MS,
};
use audio_engine_core::analysis::LoudnessMeter;
use audio_engine_core::decoder::{
    DecodeCancelToken, HttpCredentials, MediaLocation, StreamingDecoder,
};
use std::ops::Range;

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
    let options = options.normalized();
    let canceled = || {
        cancel_token
            .as_ref()
            .is_some_and(DecodeCancelToken::is_cancelled)
    };
    check_cancel(Some(&canceled))?;
    let mut decoder = StreamingDecoder::open_with_credentials_and_cancel(
        location,
        credentials.as_ref(),
        cancel_token.clone(),
    )?;

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

    decode_segment(
        &mut decoder,
        &mut meter,
        &mut head,
        0,
        plan.head.len(),
        Some(&canceled),
    )?;

    if let Some(tail_window) = plan.tail {
        check_cancel(Some(&canceled))?;
        decoder.seek(tail_window.start_time(sample_rate))?;
        let realized_start = decoder.current_frame();
        let skip_frames = tail_window.start.checked_sub(realized_start).ok_or(
            AutomixError::TailSeekPastStart {
                planned_frame: tail_window.start,
                realized_frame: realized_start,
            },
        )?;
        tail = AnalysisSegment::at(tail_window.start_time(sample_rate));
        decode_segment(
            &mut decoder,
            &mut meter,
            &mut tail,
            skip_frames,
            tail_window.len(),
            Some(&canceled),
        )?;
    }

    finalize_analysis(
        &options,
        duration,
        sample_rate,
        &meter,
        &head,
        &tail,
        Some(&canceled),
    )
}

fn decode_segment(
    decoder: &mut StreamingDecoder,
    meter: &mut LoudnessMeter,
    segment: &mut AnalysisSegment,
    skip_frames: u64,
    take_frames: u64,
    cancel_token: Option<&dyn Fn() -> bool>,
) -> Result<(), AutomixError> {
    let sample_rate = decoder.info().sample_rate;
    let channels = decoder.info().channels.max(1);
    let window_size = (sample_rate as usize * WINDOW_SIZE_MS / 1000).max(1);
    let mut chunk = Vec::with_capacity(window_size * channels);
    let mut analyzer = SegmentAnalyzer::new(sample_rate, channels);
    let mut skip_remaining = skip_frames;
    let mut take_remaining = take_frames;

    while take_remaining > 0 {
        check_cancel(cancel_token)?;
        chunk.clear();
        let Some(sample_count) = decoder.decode_next_into(&mut chunk)? else {
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
        analyzer.process(&chunk[sample_range], meter, segment, cancel_token)?;
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
