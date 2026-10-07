//! PCM-to-public-result fixtures for the fixed S2 contract. No recorded music.

use super::{synth_wav, TempAudio};
use crate::features::{AnalysisSegment, SegmentAnalyzer};
use crate::{analyze_automix, AutomixAnalysis, AutomixAnalysisMode, AutomixAnalysisOptions};
use audio_engine_core::analysis::LoudnessMeter;
use audio_engine_core::decoder::MediaLocation;
use std::f64::consts::TAU;

const SAMPLE_RATE: u32 = 8_000;
const REENTRY_SECONDS: f64 = 6.0;
const ANALYSIS_SECONDS: f64 = 10.0;

fn mixture(frame: u64, bass_weight: f64, high_weight: f64) -> f64 {
    let time = frame as f64 / f64::from(SAMPLE_RATE);
    bass_weight * (TAU * 100.0 * time).sin() + high_weight * (TAU * 1_000.0 * time).sin()
}

fn public_analysis(pcm: &[f64], mode: AutomixAnalysisMode) -> AutomixAnalysis {
    let wav = synth_wav(SAMPLE_RATE, pcm.len() as u64, |frame| pcm[frame as usize]);
    let source = TempAudio::wav(&wav);
    analyze_automix(
        MediaLocation::local(source.path_string()),
        None,
        AutomixAnalysisOptions {
            mode,
            max_analyze_time_sec: ANALYSIS_SECONDS,
        },
    )
    .expect("analyze deterministic S2 PCM fixture")
}

fn segment_from_pcm(pcm: &[f64]) -> AnalysisSegment {
    let mut segment = AnalysisSegment::default();
    let mut analyzer = SegmentAnalyzer::new(SAMPLE_RATE, 1);
    let mut meter = LoudnessMeter::new(1, SAMPLE_RATE).unwrap();
    // Deliberately cross both envelope and spectral publication boundaries.
    for chunk in pcm.chunks(137) {
        analyzer
            .process(chunk, &mut meter, &mut segment, None)
            .unwrap();
    }
    segment.reaches_input_end = true;
    segment
}

fn pcm_rms(samples: &[f64]) -> f64 {
    (samples.iter().map(|sample| sample * sample).sum::<f64>() / samples.len() as f64).sqrt()
}

fn assert_vocal_json_unavailable(analysis: &AutomixAnalysis) {
    assert_eq!(analysis.vocal_in_pos, None);
    assert_eq!(analysis.vocal_out_pos, None);
    assert_eq!(analysis.vocal_last_in_pos, None);
    let json = serde_json::to_value(analysis).unwrap();
    let object = json.as_object().unwrap();
    let mut vocal_keys: Vec<&str> = object
        .keys()
        .filter(|key| key.contains("vocal"))
        .map(String::as_str)
        .collect();
    vocal_keys.sort_unstable();
    assert_eq!(
        vocal_keys,
        ["vocal_in_pos", "vocal_last_in_pos", "vocal_out_pos"]
    );
    for key in vocal_keys {
        assert_eq!(object.get(key), Some(&serde_json::Value::Null));
    }
    let confidence_keys: Vec<&str> = object
        .keys()
        .filter(|key| key.contains("confidence"))
        .map(String::as_str)
        .collect();
    assert_eq!(confidence_keys, ["bpm_confidence"]);
    assert!(!object.contains_key("drop_confidence"));
}

#[test]
fn public_equal_full_rms_bass_reentry_survives_real_envelope_extraction() {
    let frames = u64::from(SAMPLE_RATE) * 10;
    let pcm: Vec<f64> = (0..frames)
        .map(|frame| {
            let in_breakdown =
                (2 * u64::from(SAMPLE_RATE)..6 * u64::from(SAMPLE_RATE)).contains(&frame);
            let (bass, high) = if in_breakdown {
                (0.1, 0.99_f64.sqrt())
            } else {
                (0.8, 0.6)
            };
            0.25 * mixture(frame, bass, high)
        })
        .collect();

    // Integer-cycle 100/1000 Hz tones are orthogonal within every 20 ms bin.
    // Both mixtures have coefficient-square sum one, hence equal total RMS.
    let reference_rms = pcm_rms(&pcm[..2 * SAMPLE_RATE as usize]);
    for samples in [
        &pcm[2 * SAMPLE_RATE as usize..6 * SAMPLE_RATE as usize],
        &pcm[6 * SAMPLE_RATE as usize..],
    ] {
        assert!((pcm_rms(samples) - reference_rms).abs() < 1e-12);
    }
    let segment = segment_from_pcm(&pcm);
    assert_eq!(segment.frames_analyzed, frames);
    assert_eq!(segment.envelope.len(), 500);
    assert_eq!(segment.low_envelope.len(), 500);
    assert_eq!(segment.partial_envelope, None);
    for &full_rms in &segment.envelope {
        assert!((f64::from(full_rms) - reference_rms).abs() < 1e-6);
    }
    // Confirm the PCM creates bass evidence through the actual 150 Hz filter;
    // this test does not manufacture a low_envelope for the detector.
    let mean = |values: &[f32]| {
        values.iter().map(|&value| f64::from(value)).sum::<f64>() / values.len() as f64
    };
    assert!(mean(&segment.low_envelope[300..400]) > 2.0 * mean(&segment.low_envelope[100..300]));

    for mode in [AutomixAnalysisMode::Head, AutomixAnalysisMode::Full] {
        let analysis = public_analysis(&pcm, mode);
        assert_eq!(analysis.analyze_window, ANALYSIS_SECONDS);
        let position = analysis.drop_pos.expect("equal-RMS bass re-entry evidence");
        // Freeze 100 ms for envelope/filter localization. A usable public
        // grid may additionally move the report by half a beat; the 1 ms
        // allowance covers deriving its period from public rounded BPM.
        let half_beat = if analysis.bpm_confidence.is_some_and(|value| value >= 0.35)
            && analysis
                .beat_grid_stability
                .is_some_and(|value| value >= 0.80)
        {
            30.0 / analysis.bpm.expect("usable grid reports BPM")
        } else {
            0.0
        };
        let tolerance = 0.101 + half_beat;
        assert!(
            (position - REENTRY_SECONDS).abs() <= tolerance,
            "{mode:?}: position={position}, expected={REENTRY_SECONDS}, tolerance={tolerance}"
        );
        assert_vocal_json_unavailable(&analysis);
    }
}

#[test]
fn public_broadband_gain_dip_and_jump_do_not_become_a_drop() {
    let pcm: Vec<f64> = (0..u64::from(SAMPLE_RATE) * 10)
        .map(|frame| {
            let in_breakdown =
                (2 * u64::from(SAMPLE_RATE)..6 * u64::from(SAMPLE_RATE)).contains(&frame);
            let gain = if in_breakdown { 0.05 } else { 0.25 };
            gain * mixture(frame, 0.8, 0.6)
        })
        .collect();
    // Both low and full amplitudes rise fivefold at six seconds. Their
    // composition is identical; a low-amplitude-ratio-only rule would err.
    let before = pcm_rms(&pcm[2 * SAMPLE_RATE as usize..6 * SAMPLE_RATE as usize]);
    let after = pcm_rms(&pcm[6 * SAMPLE_RATE as usize..]);
    assert!((after / before - 5.0).abs() < 1e-12);
    for mode in [AutomixAnalysisMode::Head, AutomixAnalysisMode::Full] {
        let analysis = public_analysis(&pcm, mode);
        assert_eq!(analysis.drop_pos, None, "{mode:?}");
        assert_vocal_json_unavailable(&analysis);
    }
}

fn pink_noise(frames: usize) -> Vec<f64> {
    // Deterministic Voss-style octave-held noise plus a white component.
    // This is a finite pink-noise test source, not a recording or a VAD label.
    fn white(state: &mut u32) -> f64 {
        *state ^= *state << 13;
        *state ^= *state >> 17;
        *state ^= *state << 5;
        2.0 * f64::from(*state) / f64::from(u32::MAX) - 1.0
    }
    let mut state = 0x7159_a83d;
    let mut octaves = [0.0; 12];
    for octave in &mut octaves {
        *octave = white(&mut state);
    }
    let mut samples: Vec<f64> = (0..frames)
        .map(|index| {
            let octave = (index + 1).trailing_zeros().min(11) as usize;
            octaves[octave] = white(&mut state);
            octaves.iter().sum::<f64>() + white(&mut state)
        })
        .collect();
    let dc = samples.iter().sum::<f64>() / frames as f64;
    let peak = samples
        .iter()
        .map(|sample| (sample - dc).abs())
        .fold(0.0, f64::max);
    for sample in &mut samples {
        *sample = (*sample - dc) / peak;
    }
    samples
}

#[test]
fn public_pink_noise_and_vocal_band_tones_keep_all_three_keys_null_at_every_level() {
    // Twelve seconds with a ten-second cap exercises both a populated head
    // and a separately decoded two-second tail in Full mode.
    let frames = SAMPLE_RATE as usize * 12;
    let pink = pink_noise(frames);
    let tone: Vec<f64> = (0..frames)
        .map(|frame| (TAU * 1_000.0 * frame as f64 / f64::from(SAMPLE_RATE)).sin())
        .collect();
    for source in [&pink, &tone] {
        for amplitude in [0.01, 0.1, 0.6] {
            let pcm: Vec<f64> = source.iter().map(|sample| sample * amplitude).collect();
            let analysis = public_analysis(&pcm, AutomixAnalysisMode::Full);
            assert_eq!(analysis.analyze_window, ANALYSIS_SECONDS);
            assert!(analysis.duration > ANALYSIS_SECONDS);
            assert!(analysis.outro_energy_level.is_some());
            assert_vocal_json_unavailable(&analysis);
        }
    }
}
