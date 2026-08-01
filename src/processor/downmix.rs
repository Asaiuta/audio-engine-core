//! Channel downmix / layout-mapping stage.
//!
//! This stage runs **before** the [`DspChain`](crate::processor::DspChain): it
//! maps a multichannel [`ChannelLayout`] down to stereo or mono using a
//! precomputed coefficient matrix, then the (unchanged, channel-count-based)
//! DSP chain processes the result. Keeping mixing here means the chain's
//! processors never need positional channel roles, and all channel-order and
//! coefficient handling lives in one auditable place.
//!
//! # Realtime safety
//!
//! All allocation and coefficient design happen in [`Downmixer::new`] (off the
//! audio thread). [`Downmixer::process_into`] is a pure matrix multiply over a
//! caller-provided output buffer: no allocation, no locks, no logging, no
//! panics. See `.trellis/spec/backend/realtime-safety.md`.
//!
//! # Coefficient sets
//!
//! Two selectable sets are provided via [`DownmixCoefficients`]; see that type
//! for the exact coefficients and their rationale. The enum is
//! `#[non_exhaustive]` so further sets can be added without breaking callers.

use crate::channel_layout::{ChannelLayout, ChannelPosition};

/// `1/√2` ≈ 0.7071 (−3 dB), the canonical center/surround downmix coefficient.
const INV_SQRT2: f64 = std::f64::consts::FRAC_1_SQRT_2;

/// Selectable downmix coefficient sets.
///
/// The matrix for the selected set is computed once in [`Downmixer::new`]; the
/// hot path only multiplies. Defaults to [`DownmixCoefficients::ItuRbs775`].
///
/// `#[non_exhaustive]`: more sets may be added later. Match arms must include a
/// wildcard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum DownmixCoefficients {
    /// **ITU-R BS.775** broadcast Lo/Ro downmix. The LFE channel is discarded.
    ///
    /// Per-source contributions to the stereo output (Lo, Ro):
    ///
    /// | Source        | → Lo      | → Ro      |
    /// |---------------|-----------|-----------|
    /// | Front L       | `1.0`     | `0.0`     |
    /// | Front R       | `0.0`     | `1.0`     |
    /// | Front C       | `0.7071`  | `0.7071`  |
    /// | LFE           | `0.0`     | `0.0`     |
    /// | Surround L    | `0.7071`  | `0.0`     |
    /// | Surround R    | `0.0`     | `0.7071`  |
    ///
    /// These are the standard BS.775 coefficients. The result is **not**
    /// normalized: a correlated full-scale `L+C+Ls` can exceed 0 dBFS, matching
    /// the broadcast convention that downstream limiting handles peaks.
    #[default]
    ItuRbs775,
    /// **ATSC A/85-style** cinema downmix with the LFE folded in and headroom
    /// management.
    ///
    /// Raw per-source contributions to (Lo, Ro) before headroom scaling:
    ///
    /// | Source        | → Lo            | → Ro            |
    /// |---------------|-----------------|-----------------|
    /// | Front L       | `1.0`           | `0.0`           |
    /// | Front R       | `0.0`           | `1.0`           |
    /// | Front C       | `0.7071` (−3dB) | `0.7071` (−3dB) |
    /// | LFE           | `0.3162` (−10dB)| `0.3162` (−10dB)|
    /// | Surround L    | `0.5` (−6dB)    | `0.0`           |
    /// | Surround R    | `0.0`           | `0.5` (−6dB)    |
    ///
    /// **Headroom management**: after the raw rows are built, the whole matrix
    /// is scaled by `1 / max_row_abs_sum` whenever that worst-case sum exceeds
    /// 1.0, so a correlated full-scale input can never exceed 0 dBFS. Lo and Ro
    /// rows are symmetric, so the single global scale preserves L/R balance.
    ///
    /// Coefficients are representative of the standard's cinema-style downmix
    /// rather than a bit-exact normative table; exact values can be added as a
    /// further variant if needed.
    AtscA85,
}

impl DownmixCoefficients {
    /// Whether this set normalizes the matrix for headroom (clip-safety).
    fn normalizes_headroom(self) -> bool {
        match self {
            DownmixCoefficients::ItuRbs775 => false,
            DownmixCoefficients::AtscA85 => true,
        }
    }

    /// Raw `(left, right)` stereo contribution for one source position, before
    /// any headroom scaling.
    fn stereo_gains(self, position: ChannelPosition) -> (f64, f64) {
        use ChannelPosition as P;
        // -10 dB and -6 dB linear gains for the ATSC fold-in.
        const LFE_FOLD: f64 = 0.316_227_766_016_837_94; // 10^(-10/20)
        const SURR_ATTEN: f64 = 0.5; // -6 dB

        match self {
            DownmixCoefficients::ItuRbs775 => match position {
                P::FrontLeft => (1.0, 0.0),
                P::FrontRight => (0.0, 1.0),
                P::FrontCenter => (INV_SQRT2, INV_SQRT2),
                P::LowFrequency => (0.0, 0.0),
                P::RearLeft | P::SideLeft => (INV_SQRT2, 0.0),
                P::RearRight | P::SideRight => (0.0, INV_SQRT2),
                P::FrontLeftCenter => (INV_SQRT2, 0.0),
                P::FrontRightCenter => (0.0, INV_SQRT2),
                P::RearCenter => (0.5, 0.5),
                P::Unspecified => (0.0, 0.0),
            },
            DownmixCoefficients::AtscA85 => match position {
                P::FrontLeft => (1.0, 0.0),
                P::FrontRight => (0.0, 1.0),
                P::FrontCenter => (INV_SQRT2, INV_SQRT2),
                P::LowFrequency => (LFE_FOLD, LFE_FOLD),
                P::RearLeft | P::SideLeft => (SURR_ATTEN, 0.0),
                P::RearRight | P::SideRight => (0.0, SURR_ATTEN),
                P::FrontLeftCenter => (SURR_ATTEN, 0.0),
                P::FrontRightCenter => (0.0, SURR_ATTEN),
                P::RearCenter => (SURR_ATTEN * INV_SQRT2, SURR_ATTEN * INV_SQRT2),
                P::Unspecified => (0.0, 0.0),
            },
        }
    }
}

/// Errors from constructing or running a [`Downmixer`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DownmixError {
    /// The source layout has no channels.
    #[error("downmix source layout has no channels")]
    EmptySource,
    /// The target layout is neither mono (1) nor stereo (2).
    #[error("unsupported downmix target: {channels} channel(s); only mono (1) or stereo (2)")]
    UnsupportedTarget {
        /// The offending target channel count.
        channels: usize,
    },
    /// Input length is not a whole number of source frames.
    #[error("input length {len} is not a multiple of source channel count {channels}")]
    InputNotFrameAligned {
        /// Input sample count.
        len: usize,
        /// Source channel count.
        channels: usize,
    },
    /// The output buffer cannot hold the downmixed frames.
    #[error("output buffer too small: need {needed} samples, got {got}")]
    OutputTooSmall {
        /// Required output sample count.
        needed: usize,
        /// Provided output sample count.
        got: usize,
    },
}

/// Layout-aware downmix stage with a precomputed coefficient matrix.
///
/// Maps `source` layout audio to a `target` layout (stereo or mono). Build once
/// with [`Downmixer::new`], then call [`Downmixer::process_into`] per buffer on
/// the audio thread.
#[derive(Debug)]
pub struct Downmixer {
    source: ChannelLayout,
    target: ChannelLayout,
    coefficients: DownmixCoefficients,
    /// Row-major `target_channels × source_channels` matrix:
    /// `matrix[o * src_ch + i]` is the gain from source channel `i` to output
    /// channel `o`. Allocated once here, read-only on the hot path.
    matrix: Vec<f64>,
    src_ch: usize,
    dst_ch: usize,
}

impl Downmixer {
    /// Build a downmixer mapping `source` to `target` using `coefficients`.
    ///
    /// `target` must be mono (1 channel) or stereo (2 channels). The
    /// coefficient matrix is computed here (allocation allowed); the hot path
    /// only multiplies.
    ///
    /// # Errors
    ///
    /// - [`DownmixError::EmptySource`] if `source` has no channels.
    /// - [`DownmixError::UnsupportedTarget`] if `target` is not mono or stereo.
    pub fn new(
        source: ChannelLayout,
        target: ChannelLayout,
        coefficients: DownmixCoefficients,
    ) -> Result<Self, DownmixError> {
        let src_ch = source.channel_count();
        let dst_ch = target.channel_count();

        if src_ch == 0 {
            return Err(DownmixError::EmptySource);
        }
        if dst_ch != 1 && dst_ch != 2 {
            return Err(DownmixError::UnsupportedTarget { channels: dst_ch });
        }

        let matrix = build_matrix(&source, dst_ch, coefficients);

        Ok(Self {
            source,
            target,
            coefficients,
            matrix,
            src_ch,
            dst_ch,
        })
    }

    /// The source layout.
    pub fn source(&self) -> &ChannelLayout {
        &self.source
    }

    /// The target layout.
    pub fn target(&self) -> &ChannelLayout {
        &self.target
    }

    /// The active coefficient set.
    pub fn coefficients(&self) -> DownmixCoefficients {
        self.coefficients
    }

    /// Source channel count.
    pub fn source_channels(&self) -> usize {
        self.src_ch
    }

    /// Target channel count.
    pub fn target_channels(&self) -> usize {
        self.dst_ch
    }

    /// The row-major coefficient matrix (`target × source`). Exposed for
    /// inspection and testing.
    pub fn matrix(&self) -> &[f64] {
        &self.matrix
    }

    /// Output sample count produced for an input of `input_len` samples.
    pub fn output_len(&self, input_len: usize) -> usize {
        (input_len / self.src_ch) * self.dst_ch
    }

    /// Downmix interleaved `input` into interleaved `output`.
    ///
    /// Returns the number of frames written. `output` must hold at least
    /// [`output_len`](Self::output_len)`(input.len())` samples; any trailing
    /// capacity beyond that is left untouched.
    ///
    /// Realtime-safe: no allocation, no locks, no logging, no panics.
    ///
    /// # Errors
    ///
    /// - [`DownmixError::InputNotFrameAligned`] if `input.len()` is not a
    ///   multiple of the source channel count.
    /// - [`DownmixError::OutputTooSmall`] if `output` cannot hold the result.
    pub fn process_into(&self, input: &[f64], output: &mut [f64]) -> Result<usize, DownmixError> {
        let src_ch = self.src_ch;
        let dst_ch = self.dst_ch;

        if !input.len().is_multiple_of(src_ch) {
            return Err(DownmixError::InputNotFrameAligned {
                len: input.len(),
                channels: src_ch,
            });
        }

        let frames = input.len() / src_ch;
        let needed = frames * dst_ch;
        if output.len() < needed {
            return Err(DownmixError::OutputTooSmall {
                needed,
                got: output.len(),
            });
        }

        // Pure matrix multiply. `chunks_exact` over a frame-aligned input never
        // leaves a remainder; zipping bounds the output to exactly `frames`
        // frames, so no indexing can go out of range (no panic).
        let matrix = &self.matrix;
        for (in_frame, out_frame) in input
            .chunks_exact(src_ch)
            .zip(output.chunks_exact_mut(dst_ch))
        {
            for (o, out) in out_frame.iter_mut().enumerate() {
                let row = &matrix[o * src_ch..o * src_ch + src_ch];
                let mut acc = 0.0;
                for (coeff, &sample) in row.iter().zip(in_frame.iter()) {
                    acc += coeff * sample;
                }
                *out = acc;
            }
        }

        Ok(frames)
    }
}

/// Build the row-major `dst_ch × src_ch` coefficient matrix.
///
/// `dst_ch` is 1 (mono) or 2 (stereo) — validated by the caller. For mono, the
/// single row is the power-preserving fold `M = (Lo + Ro) / √2`, so a centered
/// source passes at unity and a hard-panned source lands at −3 dB.
fn build_matrix(source: &ChannelLayout, dst_ch: usize, set: DownmixCoefficients) -> Vec<f64> {
    let src_ch = source.channel_count();
    let mut matrix = vec![0.0; dst_ch * src_ch];

    for (i, &position) in source.positions().iter().enumerate() {
        let (left, right) = set.stereo_gains(position);
        if dst_ch == 2 {
            matrix[i] = left; // row 0 (Lo)
            matrix[src_ch + i] = right; // row 1 (Ro)
        } else {
            matrix[i] = (left + right) * INV_SQRT2; // mono fold
        }
    }

    if set.normalizes_headroom() {
        let mut max_row_sum = 0.0_f64;
        for o in 0..dst_ch {
            let row_sum: f64 = matrix[o * src_ch..o * src_ch + src_ch]
                .iter()
                .map(|c| c.abs())
                .sum();
            max_row_sum = max_row_sum.max(row_sum);
        }
        if max_row_sum > 1.0 {
            let gain = 1.0 / max_row_sum;
            for coeff in matrix.iter_mut() {
                *coeff *= gain;
            }
        }
    }

    matrix
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-9;

    fn frame(input: &[f64], downmixer: &Downmixer) -> Vec<f64> {
        let mut out = vec![0.0; downmixer.output_len(input.len())];
        downmixer.process_into(input, &mut out).unwrap();
        out
    }

    #[test]
    fn default_coefficients_is_itu() {
        assert_eq!(
            DownmixCoefficients::default(),
            DownmixCoefficients::ItuRbs775
        );
    }

    #[test]
    fn unsupported_target_rejected() {
        let err = Downmixer::new(
            ChannelLayout::surround_5_1(),
            ChannelLayout::surround_5_1(),
            DownmixCoefficients::ItuRbs775,
        )
        .unwrap_err();
        assert_eq!(err, DownmixError::UnsupportedTarget { channels: 6 });
    }

    #[test]
    fn empty_source_rejected() {
        let err = Downmixer::new(
            ChannelLayout::from_count(0),
            ChannelLayout::stereo(),
            DownmixCoefficients::ItuRbs775,
        )
        .unwrap_err();
        assert_eq!(err, DownmixError::EmptySource);
    }

    #[test]
    fn input_must_be_frame_aligned() {
        let dm = Downmixer::new(
            ChannelLayout::surround_5_1(),
            ChannelLayout::stereo(),
            DownmixCoefficients::ItuRbs775,
        )
        .unwrap();
        let mut out = vec![0.0; 8];
        let err = dm.process_into(&[0.0; 5], &mut out).unwrap_err();
        assert_eq!(
            err,
            DownmixError::InputNotFrameAligned {
                len: 5,
                channels: 6
            }
        );
    }

    #[test]
    fn output_too_small_rejected() {
        let dm = Downmixer::new(
            ChannelLayout::surround_5_1(),
            ChannelLayout::stereo(),
            DownmixCoefficients::ItuRbs775,
        )
        .unwrap();
        let mut out = vec![0.0; 1];
        let err = dm.process_into(&[0.0; 6], &mut out).unwrap_err();
        assert_eq!(err, DownmixError::OutputTooSmall { needed: 2, got: 1 });
    }

    #[test]
    fn itu_5_1_to_stereo_known_coefficients() {
        let dm = Downmixer::new(
            ChannelLayout::surround_5_1(),
            ChannelLayout::stereo(),
            DownmixCoefficients::ItuRbs775,
        )
        .unwrap();

        // One 5.1 frame: L R C LFE Ls Rs
        let input = [1.0, 2.0, 4.0, 9.0, 8.0, 16.0];
        let out = frame(&input, &dm);

        // Lo = L + 0.7071*C + 0.7071*Ls; LFE discarded.
        let lo = 1.0 + INV_SQRT2 * 4.0 + INV_SQRT2 * 8.0;
        // Ro = R + 0.7071*C + 0.7071*Rs.
        let ro = 2.0 + INV_SQRT2 * 4.0 + INV_SQRT2 * 16.0;
        assert!((out[0] - lo).abs() < EPS, "Lo {} vs {}", out[0], lo);
        assert!((out[1] - ro).abs() < EPS, "Ro {} vs {}", out[1], ro);
    }

    #[test]
    fn itu_discards_lfe_atsc_folds_it_in() {
        // Signal only in the LFE channel of a 5.1 frame.
        let input = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0];

        let itu = Downmixer::new(
            ChannelLayout::surround_5_1(),
            ChannelLayout::stereo(),
            DownmixCoefficients::ItuRbs775,
        )
        .unwrap();
        let itu_out = frame(&input, &itu);
        assert!(
            itu_out[0].abs() < EPS && itu_out[1].abs() < EPS,
            "ITU keeps LFE out"
        );

        let atsc = Downmixer::new(
            ChannelLayout::surround_5_1(),
            ChannelLayout::stereo(),
            DownmixCoefficients::AtscA85,
        )
        .unwrap();
        let atsc_out = frame(&input, &atsc);
        assert!(
            atsc_out[0].abs() > EPS && atsc_out[1].abs() > EPS,
            "ATSC folds LFE in: {atsc_out:?}"
        );
    }

    #[test]
    fn atsc_is_headroom_safe_while_itu_can_clip() {
        // Correlated full-scale 5.1 frame: L=R=C=Ls=Rs=1.0 (LFE=1.0 too).
        let input = [1.0, 1.0, 1.0, 1.0, 1.0, 1.0];

        let itu = Downmixer::new(
            ChannelLayout::surround_5_1(),
            ChannelLayout::stereo(),
            DownmixCoefficients::ItuRbs775,
        )
        .unwrap();
        let itu_out = frame(&input, &itu);
        // ITU: 1 + 0.7071 + 0.7071 = 2.414 > 1.0 (unnormalized).
        assert!(itu_out[0] > 1.0, "ITU exceeds unity: {}", itu_out[0]);

        let atsc = Downmixer::new(
            ChannelLayout::surround_5_1(),
            ChannelLayout::stereo(),
            DownmixCoefficients::AtscA85,
        )
        .unwrap();
        let atsc_out = frame(&input, &atsc);
        // ATSC: headroom-managed, never exceeds 0 dBFS for in-range input.
        assert!(
            atsc_out[0] <= 1.0 + EPS,
            "ATSC stays <= unity: {}",
            atsc_out[0]
        );
        assert!(
            atsc_out[1] <= 1.0 + EPS,
            "ATSC stays <= unity: {}",
            atsc_out[1]
        );
        // Predictable difference: ATSC is quieter than ITU for the same input.
        assert!(atsc_out[0] < itu_out[0]);
    }

    #[test]
    fn channel_order_correctness_5_1_to_stereo() {
        // A signal placed in exactly one source channel must land in the
        // expected output channel(s).
        let dm = Downmixer::new(
            ChannelLayout::surround_5_1(),
            ChannelLayout::stereo(),
            DownmixCoefficients::ItuRbs775,
        )
        .unwrap();

        // Front-left only -> Lo only.
        let out = frame(&[1.0, 0.0, 0.0, 0.0, 0.0, 0.0], &dm);
        assert!(out[0] > EPS && out[1].abs() < EPS, "FL -> Lo only: {out:?}");

        // Rear-right (index 5) only -> Ro only.
        let out = frame(&[0.0, 0.0, 0.0, 0.0, 0.0, 1.0], &dm);
        assert!(out[1] > EPS && out[0].abs() < EPS, "Rs -> Ro only: {out:?}");

        // Center (index 2) -> both equally.
        let out = frame(&[0.0, 0.0, 1.0, 0.0, 0.0, 0.0], &dm);
        assert!(
            (out[0] - out[1]).abs() < EPS && out[0] > EPS,
            "C -> both: {out:?}"
        );
    }

    #[test]
    fn channel_order_correctness_7_1_to_stereo() {
        let dm = Downmixer::new(
            ChannelLayout::surround_7_1(),
            ChannelLayout::stereo(),
            DownmixCoefficients::ItuRbs775,
        )
        .unwrap();

        // 7.1: L R C LFE Ls Rs SL SR. Side-left (index 6) -> Lo only.
        let mut input = vec![0.0; 8];
        input[6] = 1.0;
        let out = frame(&input, &dm);
        assert!(out[0] > EPS && out[1].abs() < EPS, "SL -> Lo only: {out:?}");

        // Side-right (index 7) -> Ro only.
        let mut input = vec![0.0; 8];
        input[7] = 1.0;
        let out = frame(&input, &dm);
        assert!(out[1] > EPS && out[0].abs() < EPS, "SR -> Ro only: {out:?}");
    }

    #[test]
    fn unspecified_channels_never_gain_guessed_downmix_roles() {
        let layout = ChannelLayout::from_positions([
            ChannelPosition::FrontLeft,
            ChannelPosition::FrontRight,
            ChannelPosition::Unspecified,
            ChannelPosition::Unspecified,
        ]);

        for coefficients in [DownmixCoefficients::ItuRbs775, DownmixCoefficients::AtscA85] {
            let downmixer =
                Downmixer::new(layout.clone(), ChannelLayout::stereo(), coefficients).unwrap();
            let output = frame(&[0.0, 0.0, 1.0, -1.0], &downmixer);
            assert_eq!(output, [0.0, 0.0]);
        }
    }

    #[test]
    fn mono_fold_is_power_preserving_for_center() {
        let dm = Downmixer::new(
            ChannelLayout::surround_5_1(),
            ChannelLayout::mono(),
            DownmixCoefficients::ItuRbs775,
        )
        .unwrap();
        // Center-only at unity should pass at ~unity in mono.
        let out = frame(&[0.0, 0.0, 1.0, 0.0, 0.0, 0.0], &dm);
        assert_eq!(out.len(), 1);
        assert!(
            (out[0] - 1.0).abs() < EPS,
            "centered mono fold = {}",
            out[0]
        );
    }

    #[test]
    fn stereo_to_stereo_is_identity_passthrough() {
        let dm = Downmixer::new(
            ChannelLayout::stereo(),
            ChannelLayout::stereo(),
            DownmixCoefficients::AtscA85,
        )
        .unwrap();
        let input = [0.25, -0.5, 0.75, -1.0];
        let out = frame(&input, &dm);
        assert_eq!(out, input.to_vec());
    }

    #[test]
    fn process_into_is_steady_state_no_alloc() {
        let dm = Downmixer::new(
            ChannelLayout::surround_7_1(),
            ChannelLayout::stereo(),
            DownmixCoefficients::AtscA85,
        )
        .unwrap();
        let input = vec![0.3_f64; 8 * 512];
        let mut output = vec![0.0_f64; dm.output_len(input.len())];

        assert_no_alloc::assert_no_alloc(|| {
            for _ in 0..1_000 {
                dm.process_into(&input, &mut output).unwrap();
            }
        });
    }

    #[test]
    fn multichannel_layouts_all_downmix() {
        for layout in [
            ChannelLayout::mono(),
            ChannelLayout::stereo(),
            ChannelLayout::surround_5_1(),
            ChannelLayout::surround_7_1(),
        ] {
            for target in [ChannelLayout::stereo(), ChannelLayout::mono()] {
                for set in [DownmixCoefficients::ItuRbs775, DownmixCoefficients::AtscA85] {
                    let dm = Downmixer::new(layout.clone(), target.clone(), set).unwrap();
                    let frames = 32;
                    let input = vec![0.1_f64; frames * layout.channel_count()];
                    let out = frame(&input, &dm);
                    assert_eq!(out.len(), frames * target.channel_count());
                    assert!(out.iter().all(|s| s.is_finite()));
                }
            }
        }
    }
}
