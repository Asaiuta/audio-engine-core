//! Sustained sound/silence boundaries (Silence v2).
//!
//! One [`SoundBoundaries`] result feeds `fade_in_pos`, `fade_out_pos`, the
//! cut/mix seeds derived from them, and `outro_energy_level`, so those fields
//! cannot disagree about where sound starts or ends.
//!
//! Each analyzed window is scanned on its own nominal 20 ms RMS envelope. Every
//! bin is classified as *sound* (at or above [`SOUND_LATCH_DB`]), *silent* (at
//! or below [`SILENCE_LATCH_DB`]) or *dead band* (between them, or any
//! non-finite value):
//!
//! * A window starts with no sound latched. Sound latches after
//!   100 ms of complete sound bins. Once sound is latched, silence latches after
//!   2 s of complete silent bins. The counts are rounded up using the actual
//!   PCM/bin cadence (5/100 at exactly 50 Hz). Any other bin keeps the state and restarts the
//!   unconfirmed run, so a dead-band bin interrupts both kinds of run.
//! * A confirmed transition is reported at the *start* of its qualifying run,
//!   not at the later bin where the run became long enough.
//! * End of input: an unconfirmed sound run at the last bin is discarded (it is
//!   indistinguishable from a click). An unconfirmed silent run that reaches the
//!   last bin is accepted only when the window ends where the input ends (the
//!   known track end, or decoder end of stream), and any partial final bin is
//!   also silent. Partial bins never complete a confirmation run. A window that stops at the analysis cap does not
//!   confirm it.
//! * Short windows: a window with fewer bins than a confirmation run can only
//!   latch through the end-of-input rule above; otherwise it has no boundary.
//! * Windows are never joined. The Full-mode tail is scanned from its own
//!   origin, so neither state nor a run crosses the unanalyzed middle interval
//!   (or the seam of contiguous windows).

use crate::features::AnalysisSegment;
use crate::{check_cancel, AutomixError};

/// Sound latches at or above this 20 ms RMS level, in dBFS.
pub(super) const SOUND_LATCH_DB: f32 = -48.0;
/// With sound latched, silence latches at or below this level, in dBFS.
pub(super) const SILENCE_LATCH_DB: f32 = -54.0;
const SOUND_CONFIRM_SEC: f64 = 0.1;
const SILENCE_CONFIRM_SEC: f64 = 2.0;
const CANCEL_POLL_BINS: usize = 4_096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SoundEnd {
    /// First complete-bin index of the silent run, or the partial-bin origin.
    Silence(usize),
    /// Sound remains latched through the actual selected PCM end.
    WindowEnd,
}

impl SoundEnd {
    fn complete_bins(self, len: usize) -> usize {
        match self {
            Self::Silence(bin) => bin,
            Self::WindowEnd => len,
        }
    }
}

/// Latched sound evidence for one analyzed window, in envelope-bin indices.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct WindowSound {
    /// First bin of the first confirmed sound run.
    pub(super) first_sound: Option<usize>,
    /// Silence boundary or actual selected end; absent if sound never latched.
    pub(super) sound_end: Option<SoundEnd>,
}

/// Scan one window's envelope with the Silence v2 state machine.
///
/// `reaches_input_end` must be true only when the window's last selected frame
/// is the end of the input: the known track end or decoder end of stream.
pub(super) fn scan_window(
    envelope: &[f32],
    partial: Option<f32>,
    reaches_input_end: bool,
    rate: f64,
    cancel: Option<&dyn Fn() -> bool>,
) -> Result<WindowSound, AutomixError> {
    check_cancel(cancel)?;
    let sound_level = 10.0_f32.powf(SOUND_LATCH_DB / 20.0);
    let silence_level = 10.0_f32.powf(SILENCE_LATCH_DB / 20.0);
    let sound_bins = (SOUND_CONFIRM_SEC * rate).ceil() as usize;
    let silence_bins = (SILENCE_CONFIRM_SEC * rate).ceil() as usize;
    let mut result = WindowSound::default();
    let mut sound_latched = false;
    let mut run_start = 0;
    let mut run_len = 0;

    for (index, &value) in envelope.iter().enumerate() {
        if index % CANCEL_POLL_BINS == 0 {
            check_cancel(cancel)?;
        }
        // Only the class that could change the latched state extends a run.
        // Non-finite bins are dead band: they hold the state and break runs.
        let extends_run = value.is_finite()
            && if sound_latched {
                value <= silence_level
            } else {
                value >= sound_level
            };
        if !extends_run {
            run_len = 0;
            continue;
        }
        if run_len == 0 {
            run_start = index;
        }
        run_len += 1;
        if sound_latched && run_len >= silence_bins {
            result.sound_end = Some(SoundEnd::Silence(run_start));
            sound_latched = false;
            run_len = 0;
        } else if !sound_latched && run_len >= sound_bins {
            result.first_sound.get_or_insert(run_start);
            sound_latched = true;
            run_len = 0;
        }
    }

    if sound_latched {
        let partial_silent = partial.is_some_and(|v| v.is_finite() && v <= silence_level);
        let silence_reaches_end = partial.is_none() || partial_silent;
        result.sound_end = Some(if reaches_input_end && silence_reaches_end && run_len > 0 {
            SoundEnd::Silence(run_start)
        } else if reaches_input_end && partial_silent {
            // A silent partial bin can end already-latched sound at EOF, but
            // cannot establish sound or complete a normal duration threshold.
            SoundEnd::Silence(envelope.len())
        } else {
            SoundEnd::WindowEnd
        });
    }
    Ok(result)
}

/// Shared boundary result consumed by every fade/cut/mix/outro field.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct SoundBoundaries {
    /// Absolute start of the first confirmed sound in the head, else the head
    /// origin.
    pub(super) fade_in: f64,
    /// Absolute end of the last latched sound in the tail (Full mode with a
    /// tail) or else the head, capped at `duration`; `duration` when that
    /// window never latched sound.
    pub(super) fade_out: f64,
    /// Exclusive tail-envelope bin that ends outro evidence; `None` without a
    /// tail. Equals the tail's `sound_end`, or the tail length without sound.
    pub(super) outro_end_bin: Option<usize>,
}

/// Compute the one boundary result shared by fades, cuts, the mix window and
/// outro energy. Each window supplies its actual PCM/bin cadence.
pub(super) fn sound_boundaries(
    head: &AnalysisSegment,
    tail: Option<&AnalysisSegment>,
    duration: f64,
    cancel: Option<&dyn Fn() -> bool>,
) -> Result<SoundBoundaries, AutomixError> {
    let scan = |segment: &AnalysisSegment| {
        scan_window(
            &segment.envelope,
            segment.partial_envelope,
            segment.reaches_input_end,
            segment.envelope_rate(),
            cancel,
        )
    };
    let head_scan = scan(head)?;
    let fade_in = head_scan.first_sound.map_or(head.start_time, |bin| {
        head.start_time + bin as f64 / head.envelope_rate()
    });
    let (out_segment, out_scan) = match tail {
        Some(tail) => (tail, scan(tail)?),
        None => (head, head_scan),
    };
    let fade_out = out_scan
        .sound_end
        .map_or(duration, |end| match end {
            SoundEnd::Silence(bin) => {
                out_segment.start_time + bin as f64 / out_segment.envelope_rate()
            }
            SoundEnd::WindowEnd => out_segment.end_time(),
        })
        .min(duration);
    let outro_end_bin = tail.map(|segment| {
        out_scan.sound_end.map_or(segment.envelope.len(), |end| {
            end.complete_bins(segment.envelope.len())
        })
    });
    Ok(SoundBoundaries {
        fade_in,
        fade_out,
        outro_end_bin,
    })
}

#[cfg(test)]
mod tests;
