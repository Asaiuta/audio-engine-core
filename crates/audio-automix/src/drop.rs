//! Corroborated bass breakdown/re-entry evidence over complete envelope bins.
//!
//! A candidate needs two seconds of earlier bass, four seconds of breakdown
//! and two seconds of re-entry. This deliberately abstains on an eight-second
//! support deficit, including the ordinary five-second analysis setting.
//! Thresholds define a conservative synthetic contract, not music accuracy.

use super::snap_to_beat;
use crate::features::AnalysisSegment;
use crate::tempo::TempoEstimate;
use crate::{check_cancel, AutomixError};
use std::collections::VecDeque;

const REFERENCE_SECONDS: f64 = 2.0;
const BREAKDOWN_SECONDS: f64 = 4.0;
const POST_SECONDS: f64 = 2.0;
const SUSTAINED_DIP_SECONDS: f64 = 1.0;
const MIN_MEAN_AMPLITUDE: f64 = 0.001;
const MIN_LOW_RISE: f64 = 2.0;
const MIN_FULL_RETENTION: f64 = 0.8;
const MIN_SHARE_RISE: f64 = 1.5;
const MAX_BREAKDOWN_SHARE: f64 = 0.5;
const CANCEL_POLL_BINS: usize = 2_048;

#[derive(Clone, Copy, Default)]
struct Sum {
    full: f64,
    low: f64,
}

impl Sum {
    fn mean(prefix: &[Self], start: usize, end: usize) -> Self {
        let count = (end - start) as f64;
        Self {
            full: (prefix[end].full - prefix[start].full) / count,
            low: (prefix[end].low - prefix[start].low) / count,
        }
    }

    fn share(self) -> f64 {
        self.low / self.full
    }
}

/// Find head-only evidence and optionally snap it to an individual usable beat.
/// The caller supplies the cadence of complete bins; no partial EOF bin may
/// count toward the required temporal support. The source origin is absolute.
pub(super) fn detect_drop(
    head: &AnalysisSegment,
    rate: f64,
    tempo: TempoEstimate,
    cancel: Option<&dyn Fn() -> bool>,
) -> Result<Option<f64>, AutomixError> {
    check_cancel(cancel)?;
    if !head.start_time.is_finite() || head.start_time < 0.0 {
        return Ok(None);
    }
    let Some(index) = candidate(&head.envelope, &head.low_envelope, rate, cancel)? else {
        return Ok(None);
    };
    let time = head.start_time + index as f64 / rate;
    let end = head.end_time();
    if !time.is_finite() || !end.is_finite() || time < head.start_time || time >= end {
        return Ok(None);
    }
    let grid = tempo.usable_grid().filter(|grid| {
        grid.period_sec.is_finite() && grid.period_sec > 0.0 && grid.first_beat_sec.is_finite()
    });
    Ok(Some(grid.map_or(time, |grid| {
        let snapped = snap_to_beat(time, grid);
        if snapped.is_finite() && snapped >= head.start_time && snapped < end {
            snapped
        } else {
            time
        }
    })))
}

fn candidate(
    full: &[f32],
    low: &[f32],
    rate: f64,
    cancel: Option<&dyn Fn() -> bool>,
) -> Result<Option<usize>, AutomixError> {
    // Bound the float-to-integer conversions by the actual input length first.
    if full.len() != low.len()
        || !rate.is_finite()
        || rate <= 0.0
        || rate > full.len() as f64 / (REFERENCE_SECONDS + BREAKDOWN_SECONDS + POST_SECONDS)
    {
        return Ok(None);
    }
    let reference_len = (REFERENCE_SECONDS * rate).ceil() as usize;
    let breakdown_len = (BREAKDOWN_SECONDS * rate).ceil() as usize;
    let post_len = (POST_SECONDS * rate).ceil() as usize;
    let dip_len = (SUSTAINED_DIP_SECONDS * rate).ceil() as usize;
    let lookback = reference_len + breakdown_len;
    if lookback + post_len > full.len() {
        return Ok(None);
    }

    // Every bin enters one f64 prefix sum. A non-finite/negative source bin
    // invalidates the window instead of becoming a zero-energy observation.
    let mut prefix = Vec::with_capacity(full.len() + 1);
    let mut sum = Sum::default();
    prefix.push(sum);
    for (index, (&full, &low)) in full.iter().zip(low).enumerate() {
        if index % CANCEL_POLL_BINS == 0 {
            check_cancel(cancel)?;
        }
        if !full.is_finite() || full < 0.0 || !low.is_finite() || low < 0.0 {
            return Ok(None);
        }
        sum.full += f64::from(full);
        sum.low += f64::from(low);
        prefix.push(sum);
    }

    // A monotone queue gives the maximum low/full ratio in the preceding
    // second. Each bin enters/leaves at most once, so the full scan is O(n).
    let mut maxima: VecDeque<(usize, f64)> = VecDeque::with_capacity(dip_len);
    let mut best: Option<(usize, f64)> = None;
    let last_candidate = full.len() - post_len;
    for index in 0..last_candidate {
        if index % CANCEL_POLL_BINS == 0 {
            check_cancel(cancel)?;
        }
        let time = index + 1;
        while maxima.front().is_some_and(|&(bin, _)| bin + dip_len < time) {
            maxima.pop_front();
        }
        let ratio = if full[index] > 0.0 {
            f64::from(low[index]) / f64::from(full[index])
        } else if low[index] == 0.0 {
            0.0
        } else {
            // A filter tail over a zero full-band bin is not a bass dip.
            f64::INFINITY
        };
        while maxima.back().is_some_and(|&(_, value)| value <= ratio) {
            maxima.pop_back();
        }
        maxima.push_back((index, ratio));
        if time < lookback {
            continue;
        }

        let reference = Sum::mean(&prefix, time - lookback, time - breakdown_len);
        let breakdown = Sum::mean(&prefix, time - breakdown_len, time);
        let post = Sum::mean(&prefix, time, time + post_len);
        if reference.full < MIN_MEAN_AMPLITUDE
            || breakdown.full < MIN_MEAN_AMPLITUDE
            || post.full < MIN_MEAN_AMPLITUDE
            || reference.low < MIN_MEAN_AMPLITUDE
            || post.low < MIN_MEAN_AMPLITUDE
            || post.low < MIN_LOW_RISE * breakdown.low
            || post.full < MIN_FULL_RETENTION * breakdown.full
        {
            continue;
        }
        let reference_share = reference.share();
        let breakdown_share = breakdown.share();
        let post_share = post.share();
        if post_share < MIN_SHARE_RISE * breakdown_share
            || breakdown_share > MAX_BREAKDOWN_SHARE * reference_share
            || maxima
                .front()
                .is_none_or(|&(_, maximum)| maximum > MAX_BREAKDOWN_SHARE * reference_share)
        {
            continue;
        }
        let prominence = post_share - breakdown_share;
        // Strict improvement preserves the earliest candidate on an exact tie.
        if best.is_none_or(|(_, score)| prominence > score) {
            best = Some((time, prominence));
        }
    }
    check_cancel(cancel)?;
    Ok(best.map(|(index, _)| index))
}

#[cfg(test)]
#[path = "drop/tests.rs"]
mod tests;
