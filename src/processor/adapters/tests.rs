use super::*;
use crate::processor::loudness::LimiterMode;
use crate::processor::traits::AudioBlockRef;

struct TestProgress(ProcessProgress);

impl TestProgress {
    fn is_bypassed(&self) -> bool {
        self.0.is_bypassed()
    }
}

macro_rules! impl_test_process_block {
        ($($processor:ty),+ $(,)?) => {
            $(
                impl $processor {
                    fn process(
                        &mut self,
                        buffer: &mut [f64],
                        channels: usize,
                    ) -> TestProgress {
                        let block = AudioBlockMut::new(buffer, channels).unwrap();
                        TestProgress(
                            super::super::traits::process_checked(
                                self,
                                ProcessBuffers::in_place(block),
                            )
                            .unwrap(),
                        )
                    }
                }
            )+
        };
    }

impl_test_process_block!(
    EqProcessor,
    SaturationProcessor,
    CrossfeedProcessor,
    PeakLimiterProcessor,
    VolumeProcessor,
    ConvolverProcessor,
    NoiseShaperProcessor,
);

#[test]
fn test_convolver_processor_swaps_in_and_processes() {
    let control = ConvolverControl::default();
    let mut proc = ConvolverProcessor::new(control.clone()).unwrap();
    let mut buffer = vec![1.0, 2.0, 3.0, 4.0];

    assert!(proc.process(&mut buffer, 1).is_bypassed());

    let generation = control.publish(FFTConvolver::new(&[0.5], 1));
    control.set_enabled(true);
    assert!(!proc.process(&mut buffer, 1).is_bypassed());
    assert_eq!(buffer, vec![0.5, 1.0, 1.5, 2.0]);

    let status = control.status();
    assert_eq!(status.latest_published_generation, generation);
    assert_eq!(status.latest_adopted_generation, generation);
    assert_eq!(status.adopted_kernels, 1);
    assert_eq!(status.pending_kernels, 0);
    assert!(!status.backpressured);
}

#[test]
fn test_convolver_processor_clear_disables_owned_convolver() {
    let control = ConvolverControl::new(true);
    let mut proc = ConvolverProcessor::new(control.clone()).unwrap();
    let mut buffer = vec![1.0, 2.0, 3.0, 4.0];

    control.publish(FFTConvolver::new(&[0.5], 1));
    assert!(!proc.process(&mut buffer, 1).is_bypassed());

    control.set_enabled(false);
    let mut bypassed = vec![1.0, 2.0, 3.0, 4.0];
    assert!(proc.process(&mut bypassed, 1).is_bypassed());
    assert_eq!(bypassed, vec![1.0, 2.0, 3.0, 4.0]);
    assert_eq!(control.status().pending_reclamations, 1);
    assert!(control.reclaim_retired());
    assert!(control.is_quiescent());
}

#[test]
fn convolver_publication_is_latest_wins_before_audio_withdrawal() {
    let control = ConvolverControl::new(true);
    let mut proc = ConvolverProcessor::new(control.clone()).unwrap();
    let mut buffer = vec![1.0, 2.0, 3.0, 4.0];

    let first = control.publish(FFTConvolver::new(&[0.5], 1));
    let latest = control.publish(FFTConvolver::new(&[0.25], 1));
    assert!(!proc.process(&mut buffer, 1).is_bypassed());
    assert_eq!(buffer, vec![0.25, 0.5, 0.75, 1.0]);

    let status = control.status();
    assert_eq!(first, 1);
    assert_eq!(status.latest_adopted_generation, latest);
    assert_eq!(status.adopted_kernels, 1);
    assert_eq!(status.superseded_kernels, 1);
    assert_eq!(status.pending_kernels, 0);
}

#[test]
fn convolver_disable_reports_retirement_backpressure_and_recovers() {
    let control = ConvolverControl::new(true);
    let mut proc = ConvolverProcessor::new(control.clone()).unwrap();
    let mut buffer = vec![1.0; 4];

    control.publish(FFTConvolver::new(&[1.0], 1));
    assert!(!proc.process(&mut buffer, 1).is_bypassed());
    control.publish(FFTConvolver::new(&[0.5], 1));
    control.set_enabled(false);

    assert!(proc.process(&mut buffer, 1).is_bypassed());
    assert!(proc.process(&mut buffer, 1).is_bypassed());
    let saturated = control.status();
    assert!(saturated.backpressured);
    assert_eq!(saturated.discarded_kernels, 1);
    assert_eq!(saturated.pending_reclamations, 2);

    assert!(control.reclaim_retired());
    assert!(proc.process(&mut buffer, 1).is_bypassed());
    let recovered = control.status();
    assert!(!recovered.backpressured);
    assert!(recovered.audio_idle);
    assert_eq!(recovered.pending_reclamations, 1);

    assert!(control.reclaim_retired());
    assert!(control.is_quiescent());
}

#[test]
fn convolver_processor_kernel_swap_is_allocation_free_on_audio_side() {
    let control = ConvolverControl::new(true);
    let mut proc = ConvolverProcessor::new(control.clone()).unwrap();
    let mut buffer = vec![0.3; 512];

    for _ in 0..8 {
        // Control side: publishing allocates (allowed).
        control.publish(FFTConvolver::new(&[0.5, 0.25], 1));
        // Audio side: swap-in, retirement hand-off, and processing must not
        // allocate or deallocate.
        assert_no_alloc::assert_no_alloc(|| {
            proc.process(&mut buffer, 1);
        });
        // Control side: draining performs the large deallocation.
        let _ = control.reclaim_retired();
    }

    control.publish(FFTConvolver::new(&[0.75], 1));
    control.set_enabled(false);
    assert_no_alloc::assert_no_alloc(|| {
        proc.process(&mut buffer, 1);
        proc.process(&mut buffer, 1);
    });
    assert!(control.status().backpressured);

    assert!(control.reclaim_retired());
    assert_no_alloc::assert_no_alloc(|| {
        proc.process(&mut buffer, 1);
    });
    assert!(control.reclaim_retired());

    control.set_enabled(true);
    control.publish(FFTConvolver::new(&[0.25], 1));
    assert_no_alloc::assert_no_alloc(|| {
        proc.process(&mut buffer, 1);
    });
}

#[test]
fn convolver_control_stress_remains_bounded_and_adopts_latest_generation() {
    const UPDATES: u64 = 10_000;

    let control = ConvolverControl::new(true);
    let mut proc = ConvolverProcessor::new(control.clone()).unwrap();
    let mut buffer = [1.0; 4];
    let mut latest_gain = 0.0;

    for update in 0..UPDATES {
        latest_gain = 0.25 + (update % 23) as f64 * 0.01;
        let generation = control.publish(FFTConvolver::new(&[latest_gain], 1));
        assert_eq!(generation, update + 1);

        if update % 17 == 0 {
            buffer.fill(1.0);
            assert!(!proc.process(&mut buffer, 1).is_bypassed());
            assert!((buffer[0] - latest_gain).abs() <= f64::EPSILON);
        }
        if update % 113 == 0 {
            let _ = control.reclaim_retired();
        }
    }

    buffer.fill(1.0);
    assert!(!proc.process(&mut buffer, 1).is_bypassed());
    assert!((buffer[0] - latest_gain).abs() <= f64::EPSILON);
    let _ = control.reclaim_retired();

    let burst_status = control.status();
    assert_eq!(burst_status.latest_published_generation, UPDATES);
    assert_eq!(burst_status.latest_adopted_generation, UPDATES);
    assert_eq!(
        burst_status.adopted_kernels
            + burst_status.superseded_kernels
            + burst_status.discarded_kernels,
        UPDATES
    );
    assert_eq!(burst_status.pending_kernels, 0);
    assert_eq!(burst_status.pending_reclamations, 0);

    control.publish(FFTConvolver::new(&[0.5], 1));
    control.set_enabled(false);
    assert!(proc.process(&mut buffer, 1).is_bypassed());
    assert!(proc.process(&mut buffer, 1).is_bypassed());
    let saturated = control.status();
    assert!(saturated.backpressured);
    assert_eq!(saturated.pending_reclamations, 2);

    assert!(control.reclaim_retired());
    assert!(proc.process(&mut buffer, 1).is_bypassed());
    assert!(control.reclaim_retired());
    assert!(control.is_quiescent());

    control.set_enabled(true);
    let final_generation = control.publish(FFTConvolver::new(&[0.875], 1));
    buffer.fill(1.0);
    assert!(!proc.process(&mut buffer, 1).is_bypassed());
    assert_eq!(buffer, [0.875; 4]);

    let final_status = control.status();
    assert_eq!(final_status.latest_adopted_generation, final_generation);
    assert_eq!(final_status.pending_kernels, 0);
    assert_eq!(final_status.pending_reclamations, 0);
    assert!(!final_status.backpressured);
    assert!(final_status.deferred_adoptions >= 1);
    assert_eq!(
        final_status.adopted_kernels
            + final_status.superseded_kernels
            + final_status.discarded_kernels,
        final_status.latest_published_generation
    );
}

#[test]
fn convolver_control_serializes_concurrent_publishers() {
    const PUBLISHERS: usize = 4;
    const UPDATES_PER_PUBLISHER: usize = 64;
    const TOTAL_UPDATES: usize = PUBLISHERS * UPDATES_PER_PUBLISHER;

    let control = ConvolverControl::new(true);
    let start = Arc::new(std::sync::Barrier::new(PUBLISHERS));
    let mut publishers = Vec::with_capacity(PUBLISHERS);
    for publisher in 0..PUBLISHERS {
        let control = control.clone();
        let start = Arc::clone(&start);
        publishers.push(std::thread::spawn(move || {
            start.wait();
            let mut published = Vec::with_capacity(UPDATES_PER_PUBLISHER);
            for update in 0..UPDATES_PER_PUBLISHER {
                let ordinal = publisher * UPDATES_PER_PUBLISHER + update + 1;
                let gain = ordinal as f64 / TOTAL_UPDATES as f64;
                let generation = control.publish(FFTConvolver::new(&[gain], 1));
                published.push((generation, gain));
            }
            published
        }));
    }

    let mut publications = Vec::with_capacity(TOTAL_UPDATES);
    for publisher in publishers {
        publications.extend(publisher.join().unwrap());
    }
    publications.sort_by_key(|(generation, _)| *generation);
    assert_eq!(publications.len(), TOTAL_UPDATES);
    for (index, (generation, _)) in publications.iter().enumerate() {
        assert_eq!(*generation, index as u64 + 1);
    }

    let (latest_generation, latest_gain) = publications[TOTAL_UPDATES - 1];
    let mut proc = ConvolverProcessor::new(control.clone()).unwrap();
    let mut buffer = [1.0; 4];
    assert!(!proc.process(&mut buffer, 1).is_bypassed());
    assert_eq!(buffer, [latest_gain; 4]);

    let status = control.status();
    assert_eq!(status.latest_published_generation, latest_generation);
    assert_eq!(status.latest_adopted_generation, latest_generation);
    assert_eq!(status.adopted_kernels, 1);
    assert_eq!(status.superseded_kernels, TOTAL_UPDATES as u64 - 1);
    assert_eq!(status.pending_kernels, 0);
}

#[test]
fn convolver_kernels_are_destroyed_by_control_not_audio_thread() {
    use std::sync::mpsc::sync_channel;

    let control = ConvolverControl::new(true);
    let audio_control = control.clone();
    let (command_tx, command_rx) = sync_channel::<bool>(0);
    let (ready_tx, ready_rx) = sync_channel(0);
    let (processed_tx, processed_rx) = sync_channel(0);
    let audio_thread = std::thread::spawn(move || {
        ready_tx.send(std::thread::current().id()).unwrap();
        let mut proc = ConvolverProcessor::new(audio_control).unwrap();
        let mut buffer = [1.0; 4];
        while command_rx.recv().unwrap() {
            buffer.fill(1.0);
            let _ = proc.process(&mut buffer, 1);
            processed_tx.send(()).unwrap();
        }
    });
    let audio_thread_id = ready_rx.recv().unwrap();
    let dropped_on_audio = Arc::new(AtomicBool::new(false));
    let drop_count = Arc::new(AtomicU64::new(0));
    let make_probe = || ConvolverDropProbe {
        audio_thread_id,
        dropped_on_audio: Arc::clone(&dropped_on_audio),
        drop_count: Arc::clone(&drop_count),
    };
    let process_once = || {
        command_tx.send(true).unwrap();
        processed_rx.recv().unwrap();
    };

    control.publish_with_drop_probe(FFTConvolver::new(&[1.0], 1), make_probe());
    process_once();
    control.publish_with_drop_probe(FFTConvolver::new(&[0.75], 1), make_probe());
    process_once();
    assert_eq!(drop_count.load(Ordering::Acquire), 0);
    assert!(control.reclaim_retired());

    control.publish_with_drop_probe(FFTConvolver::new(&[0.5], 1), make_probe());
    control.publish_with_drop_probe(FFTConvolver::new(&[0.25], 1), make_probe());
    process_once();
    assert_eq!(drop_count.load(Ordering::Acquire), 2);
    assert!(control.reclaim_retired());

    control.set_enabled(false);
    process_once();
    assert!(control.reclaim_retired());
    assert!(control.is_quiescent());

    command_tx.send(false).unwrap();
    audio_thread.join().unwrap();
    assert_eq!(drop_count.load(Ordering::Acquire), 4);
    assert!(!dropped_on_audio.load(Ordering::Acquire));
}

#[test]
fn test_eq_processor() {
    let params = Arc::new(AtomicEqParams::new());
    let mut proc = EqProcessor::new(2, 44100.0, Arc::clone(&params));

    // Set params from "main thread"
    let gains = [2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    params.write(&gains, true);

    // Process from "audio thread"
    let mut buffer = vec![0.5; 4096];
    let result = proc.process(&mut buffer, 2);

    assert!(!result.is_bypassed());
    // EQ gain smoothing may not boost the very first sample, but the block should change.
    assert!(buffer.iter().any(|&sample| (sample - 0.5).abs() > 1e-6));
}

#[test]
fn test_volume_processor_muted() {
    let params = Arc::new(AtomicVolumeParams::new());
    let mut proc = VolumeProcessor::new(Arc::clone(&params));

    params.set_volume(0.5);
    params.set_muted(true);

    let mut buffer = vec![1.0; 4096];
    proc.process(&mut buffer, 2);

    // Muting uses a click-free exponential fade rather than an instant hard cut.
    assert!(buffer[0] < 1.0);
    assert!(buffer[buffer.len() - 1] < 0.001);
}

#[test]
fn test_volume_processor_muted_fade_is_frame_coherent() {
    // The muted fade must decay per frame, not per sample: both channels of
    // a stereo frame must receive the identical gain. A per-sample decay
    // would give L and R different gains (inter-channel skew) and halve the
    // fade time constant.
    let params = Arc::new(AtomicVolumeParams::new());
    let mut proc = VolumeProcessor::new(Arc::clone(&params));

    params.set_muted(true);

    let channels = 2;
    let mut buffer = vec![1.0; channels * 512];
    proc.process(&mut buffer, channels);

    for frame in buffer.chunks_exact(channels) {
        assert_eq!(
            frame[0], frame[1],
            "L and R of the same frame must share one gain"
        );
    }
}

#[test]
fn test_volume_processor_writes_back_smoothed_volume() {
    let params = Arc::new(AtomicVolumeParams::new());
    let mut proc = VolumeProcessor::new(Arc::clone(&params));

    params.set_volume(0.25);
    let mut buffer = vec![1.0; 128];
    proc.process(&mut buffer, 2);

    let first_pass_volume = proc.current_volume;
    assert!(first_pass_volume < 1.0);
    assert!(first_pass_volume > 0.25);

    proc.process(&mut buffer, 2);

    assert!(proc.current_volume < first_pass_volume);
    assert!(proc.current_volume > 0.25);
}

#[test]
fn test_volume_processor_steady_state_fast_path_preserves_unity() {
    let params = Arc::new(AtomicVolumeParams::new());
    let mut proc = VolumeProcessor::new(Arc::clone(&params));
    proc.reset().unwrap();

    let mut buffer = vec![0.25, -0.5, 0.75, -1.0];
    let original = buffer.clone();

    assert!(!proc.process(&mut buffer, 2).is_bypassed());
    assert_eq!(buffer, original);
    assert_eq!(proc.current_volume, 1.0);
}

#[test]
fn test_volume_processor_steady_state_fast_path_applies_target() {
    let params = Arc::new(AtomicVolumeParams::new());
    params.set_volume(0.5);
    let mut proc = VolumeProcessor::new(Arc::clone(&params));
    proc.sync_params();
    proc.reset().unwrap();

    let mut buffer = vec![0.25, -0.5, 0.75, -1.0];

    assert!(!proc.process(&mut buffer, 2).is_bypassed());
    assert_eq!(buffer, vec![0.125, -0.25, 0.375, -0.5]);
    assert_eq!(proc.current_volume, 0.5);
}

#[test]
fn volume_lazy_settle_dc_null_residual_stays_below_snap_floor() {
    let input = vec![0.8; 32_768 * 2];

    assert_lazy_settle_residual_bounds("dc", &input, 2);
}

#[test]
fn volume_lazy_settle_sweep_null_residual_stays_below_snap_floor() {
    let input = sweep_signal(32_768, 2);

    assert_lazy_settle_residual_bounds("sweep", &input, 2);
}

#[test]
fn volume_lazy_settle_abrupt_step_null_residual_stays_below_snap_floor() {
    let input = abrupt_step_signal(32_768, 2);

    assert_lazy_settle_residual_bounds("abrupt_step", &input, 2);
}

#[test]
fn test_saturation_processor() {
    let params = Arc::new(AtomicSaturationParams::new());
    let mut proc = SaturationProcessor::new(2, Arc::clone(&params));

    params.set_drive(1.0);
    params.set_mix(1.0);
    params.set_enabled(true);

    let mut buffer = vec![0.9, 0.9];
    proc.process(&mut buffer, 2);

    // tanh(0.9 * 2) ≈ 0.96, less than input
    assert!(buffer[0].abs() < 0.9 * 2.0);
}

#[test]
fn crossfeed_processor_mix_change_preserves_filter_history() {
    let params = Arc::new(AtomicCrossfeedParams::new());
    let mut proc = CrossfeedProcessor::new(48_000.0, Arc::clone(&params));
    let mut reference = Crossfeed::with_params(48_000.0, 700.0, 0.35);
    let mut reset_reference = Crossfeed::with_params(48_000.0, 700.0, 0.35);

    let warm = hard_panned_sine(2048, 0, 48_000.0, 997.0);
    let mut proc_warm = warm.clone();
    let mut ref_warm = warm.clone();
    let mut reset_warm = warm;
    proc.process(&mut proc_warm, 2);
    reference.process(&mut ref_warm, 2);
    reset_reference.process(&mut reset_warm, 2);

    params.set_mix(0.7);
    reference.set_mix(0.7);
    reset_reference.set_mix(0.7);
    reset_reference.set_sample_rate(48_000.0, 700.0);

    let next = hard_panned_sine(256, 2048, 48_000.0, 997.0);
    let mut proc_next = next.clone();
    let mut ref_next = next.clone();
    let mut reset_next = next;
    assert!(!proc.process(&mut proc_next, 2).is_bypassed());
    reference.process(&mut ref_next, 2);
    reset_reference.process(&mut reset_next, 2);

    let max_reference_delta = max_abs_delta(&proc_next, &ref_next);
    let max_reset_delta = max_abs_delta(&proc_next, &reset_next);
    assert!(
            max_reference_delta <= 1.0e-12,
            "mix change should preserve Bauer filter state, max_reference_delta={max_reference_delta:.3e}"
        );
    assert!(
        max_reset_delta > 1.0e-4,
        "test signal should distinguish reset history, max_reset_delta={max_reset_delta:.3e}"
    );
}

#[test]
fn crossfeed_processor_cutoff_change_preserves_filter_history() {
    let params = Arc::new(AtomicCrossfeedParams::new());
    let mut proc = CrossfeedProcessor::new(48_000.0, Arc::clone(&params));
    let mut reference = Crossfeed::with_params(48_000.0, 700.0, 0.35);
    let mut reset_reference = Crossfeed::with_params(48_000.0, 700.0, 0.35);

    let warm = hard_panned_sine(2048, 0, 48_000.0, 431.0);
    let mut proc_warm = warm.clone();
    let mut reference_warm = warm.clone();
    let mut reset_warm = warm;
    proc.process(&mut proc_warm, 2);
    reference.process(&mut reference_warm, 2);
    reset_reference.process(&mut reset_warm, 2);

    params.set_cutoff(1_100.0);
    reference.set_cutoff(1_100.0);
    reset_reference.set_sample_rate(48_000.0, 1_100.0);

    let next = hard_panned_sine(512, 2048, 48_000.0, 431.0);
    let mut proc_next = next.clone();
    let mut reference_next = next.clone();
    let mut reset_next = next;
    proc.process(&mut proc_next, 2);
    reference.process(&mut reference_next, 2);
    reset_reference.process(&mut reset_next, 2);

    let max_reference_delta = max_abs_delta(&proc_next, &reference_next);
    let max_reset_delta = max_abs_delta(&proc_next, &reset_next);
    assert!(
            max_reference_delta <= 1.0e-12,
            "cutoff change should preserve and ramp Bauer state, max_reference_delta={max_reference_delta:.3e}"
        );
    assert!(
        max_reset_delta > 1.0e-4,
        "test signal should distinguish reset history, max_reset_delta={max_reset_delta:.3e}"
    );
}

#[test]
fn crossfeed_processor_steady_state_process_is_allocation_free() {
    let params = Arc::new(AtomicCrossfeedParams::new());
    let mut proc = CrossfeedProcessor::new(48_000.0, Arc::clone(&params));
    let mut buffer = hard_panned_sine(512, 0, 48_000.0, 997.0);

    proc.process(&mut buffer, 2);

    assert_no_alloc::assert_no_alloc(|| {
        for _ in 0..200 {
            proc.process(&mut buffer, 2);
        }
    });
}

#[test]
fn noise_shaper_bits_change_does_not_reset_unchanged_curve_history() {
    let params = Arc::new(AtomicNoiseShaperParams::new());
    let mut processor = NoiseShaperProcessor::new(2, 48_000, Arc::clone(&params));
    let mut reference = NoiseShaper::new(2, 48_000, 24);
    reference.set_curve(params.curve());

    let mut warm = hard_panned_sine(2048, 0, 48_000.0, 997.0);
    let mut reference_warm = warm.clone();
    processor.process(&mut warm, 2);
    reference.process(&mut reference_warm, 2);
    assert_eq!(warm, reference_warm);

    params.set_bits(16);
    reference.set_bits(16);
    let mut next = hard_panned_sine(512, 2048, 48_000.0, 997.0);
    let mut reference_next = next.clone();
    processor.process(&mut next, 2);
    reference.process(&mut reference_next, 2);

    assert_eq!(next, reference_next);
}

fn assert_lazy_settle_residual_bounds(name: &str, input: &[f64], channels: usize) {
    const RESIDUAL_DELTA_LIMIT: f64 = 2.0e-6;
    const RESIDUAL_RMS_LIMIT: f64 = 2.0e-7;

    let mut exact = input.to_vec();
    let mut lazy = input.to_vec();
    process_volume_exact_kernel(&mut exact, channels, 48_000.0, 0.25);
    process_volume_lazy_settle_kernel(
        &mut lazy,
        channels,
        48_000.0,
        0.25,
        VolumeProcessor::SETTLE_EPSILON,
    );

    let mut max_abs = 0.0_f64;
    let mut sum_sq = 0.0_f64;
    let mut max_delta = 0.0_f64;
    let mut prev_residual = 0.0_f64;

    for (idx, (left, right)) in lazy.iter().zip(&exact).enumerate() {
        let residual = left - right;
        max_abs = max_abs.max(residual.abs());
        sum_sq += residual * residual;
        if idx > 0 {
            max_delta = max_delta.max((residual - prev_residual).abs());
        }
        prev_residual = residual;
    }

    let rms = (sum_sq / input.len() as f64).sqrt();
    assert!(
        max_abs <= VolumeProcessor::SETTLE_EPSILON,
        "{name} lazy-settle max residual {max_abs:.3e} exceeds {:.3e}",
        VolumeProcessor::SETTLE_EPSILON
    );
    assert!(
        max_delta <= RESIDUAL_DELTA_LIMIT,
        "{name} lazy-settle residual delta {max_delta:.3e} exceeds {RESIDUAL_DELTA_LIMIT:.3e}"
    );
    assert!(
        rms <= RESIDUAL_RMS_LIMIT,
        "{name} lazy-settle residual rms {rms:.3e} exceeds {RESIDUAL_RMS_LIMIT:.3e}"
    );
}

fn process_volume_exact_kernel(
    buffer: &mut [f64],
    channels: usize,
    sample_rate: f64,
    target: f64,
) -> f64 {
    let smoothing_coeff = VolumeProcessor::calc_smoothing_coeff(sample_rate);
    let one_minus_coeff = 1.0 - smoothing_coeff;
    let mut current_volume = 1.0;
    let frames = buffer.len() / channels;

    for frame in 0..frames {
        current_volume += (target - current_volume) * one_minus_coeff;
        for ch in 0..channels {
            buffer[frame * channels + ch] *= current_volume;
        }
    }

    current_volume
}

fn process_volume_lazy_settle_kernel(
    buffer: &mut [f64],
    channels: usize,
    sample_rate: f64,
    target: f64,
    settle_epsilon: f64,
) -> f64 {
    let smoothing_coeff = VolumeProcessor::calc_smoothing_coeff(sample_rate);
    let one_minus_coeff = 1.0 - smoothing_coeff;
    let mut current_volume = 1.0;
    let frames = buffer.len() / channels;
    let mut frame = 0;

    while frame < frames {
        if (target - current_volume).abs() <= settle_epsilon {
            current_volume = target;
            break;
        }

        current_volume += (target - current_volume) * one_minus_coeff;
        for ch in 0..channels {
            buffer[frame * channels + ch] *= current_volume;
        }
        frame += 1;
    }

    if frame < frames && target != 1.0 {
        for sample in &mut buffer[(frame * channels)..] {
            *sample *= target;
        }
    }

    current_volume
}

fn sweep_signal(frames: usize, channels: usize) -> Vec<f64> {
    let mut out = Vec::with_capacity(frames * channels);
    let sample_rate = 48_000.0;
    let start_hz = 20.0_f64;
    let end_hz = 20_000.0_f64;
    let mut phase = 0.0_f64;

    for frame in 0..frames {
        let progress = frame as f64 / frames.saturating_sub(1).max(1) as f64;
        let hz = start_hz * (end_hz / start_hz).powf(progress);
        phase += std::f64::consts::TAU * hz / sample_rate;
        let sample = phase.sin() * 0.9;
        for ch in 0..channels {
            out.push(sample * (1.0 - ch as f64 * 0.05));
        }
    }

    out
}

fn abrupt_step_signal(frames: usize, channels: usize) -> Vec<f64> {
    let mut out = Vec::with_capacity(frames * channels);

    for frame in 0..frames {
        let sample = match frame * 4 / frames.max(1) {
            0 => 0.0,
            1 => 1.0,
            2 => -1.0,
            _ => {
                if frame % 2 == 0 {
                    1.0
                } else {
                    -1.0
                }
            }
        };
        for _ in 0..channels {
            out.push(sample);
        }
    }

    out
}

fn hard_panned_sine(
    frames: usize,
    start_frame: usize,
    sample_rate: f64,
    frequency: f64,
) -> Vec<f64> {
    let mut out = Vec::with_capacity(frames * 2);
    let omega = std::f64::consts::TAU * frequency / sample_rate;
    for frame in start_frame..start_frame + frames {
        out.push((omega * frame as f64).sin() * 0.8);
        out.push(0.0);
    }
    out
}

fn max_abs_delta(left: &[f64], right: &[f64]) -> f64 {
    left.iter()
        .zip(right)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f64::max)
}

#[test]
fn fixed_bypass_copies_out_of_place_and_reports_backpressure() {
    let params = Arc::new(AtomicEqParams::new());
    params.write(&[0.0; EQ_BANDS], false);
    let mut proc = EqProcessor::new(2, 48_000.0, params);
    let input = [0.1, -0.2, 0.3, -0.4];
    let mut output = [9.0, 9.0];

    let buffers = ProcessBuffers::out_of_place(
        AudioBlockRef::new(&input, 2).unwrap(),
        AudioBlockMut::new(&mut output, 2).unwrap(),
    )
    .unwrap();
    let progress = super::super::traits::process_checked(&mut proc, buffers).unwrap();

    assert_eq!(progress.consumed_frames(), 1);
    assert_eq!(progress.produced_frames(), 1);
    assert_eq!(progress.state(), ProcessState::NeedOutput);
    assert!(progress.is_bypassed());
    assert_eq!(output, input[..2]);
}

#[test]
fn fixed_out_of_place_matches_in_place_processing() {
    let params = Arc::new(AtomicVolumeParams::new());
    params.set_volume(0.5);
    let mut in_place = VolumeProcessor::new(Arc::clone(&params));
    let mut out_of_place = VolumeProcessor::new(params);
    in_place.reset().unwrap();
    out_of_place.reset().unwrap();

    let input = [0.25, -0.5, 0.75, -1.0];
    let mut expected = input;
    let _ = in_place.process(&mut expected, 2);
    let mut actual = [0.0; 4];
    let buffers = ProcessBuffers::out_of_place(
        AudioBlockRef::new(&input, 2).unwrap(),
        AudioBlockMut::new(&mut actual, 2).unwrap(),
    )
    .unwrap();
    let progress = super::super::traits::process_checked(&mut out_of_place, buffers).unwrap();

    assert_eq!(progress.consumed_frames(), 2);
    assert_eq!(progress.produced_frames(), 2);
    assert_eq!(progress.state(), ProcessState::NeedInput);
    assert!(!progress.is_bypassed());
    assert_eq!(actual, expected);
}

#[test]
fn fixed_finish_requires_reset_before_more_input() {
    let params = Arc::new(AtomicVolumeParams::new());
    let mut proc = VolumeProcessor::new(params);
    let mut finish_output = [0.0; 2];
    let finished = super::super::traits::finish_checked(
        &mut proc,
        AudioBlockMut::new(&mut finish_output, 2).unwrap(),
    )
    .unwrap();
    assert_eq!(finished.state(), ProcessState::Finished);

    let mut input = [0.25, -0.25];
    let block = AudioBlockMut::new(&mut input, 2).unwrap();
    assert_eq!(
        super::super::traits::process_checked(&mut proc, ProcessBuffers::in_place(block),),
        Err(ProcessError::AlreadyFinished {
            processor: "Volume",
        })
    );

    proc.reset().unwrap();
    let _ = proc.process(&mut input, 2);
}

#[test]
fn configured_channel_count_is_validated_before_processing() {
    let params = Arc::new(AtomicNoiseShaperParams::new());
    let mut proc = NoiseShaperProcessor::new(2, 48_000, params);
    let mut mono = [0.25; 4];
    let block = AudioBlockMut::new(&mut mono, 1).unwrap();

    assert_eq!(
        super::super::traits::process_checked(&mut proc, ProcessBuffers::in_place(block),),
        Err(ProcessError::ChannelCountMismatch {
            processor: "NoiseShaper",
            expected_channels: 2,
            actual_channels: 1,
        })
    );
}

#[test]
fn fixed_out_of_place_processing_is_allocation_free_after_setup() {
    let params = Arc::new(AtomicVolumeParams::new());
    params.set_volume(0.5);
    let mut proc = VolumeProcessor::new(params);
    proc.reset().unwrap();
    let input = [0.25; 512 * 2];
    let mut output = [0.0; 512 * 2];

    assert_no_alloc::assert_no_alloc(|| {
        let buffers = ProcessBuffers::out_of_place(
            AudioBlockRef::new(&input, 2).unwrap(),
            AudioBlockMut::new(&mut output, 2).unwrap(),
        )
        .unwrap();
        let _ = super::super::traits::process_checked(&mut proc, buffers).unwrap();
    });
}

#[test]
fn peak_limiter_processor_defaults_to_true_peak_mode() {
    let params = Arc::new(AtomicPeakLimiterParams::new());
    let proc = PeakLimiterProcessor::new(2, 48_000, Arc::clone(&params));
    assert_eq!(proc.limiter.mode(), LimiterMode::TruePeak);
}

#[test]
fn peak_limiter_processor_applies_mode_snapshot() {
    let params = Arc::new(AtomicPeakLimiterParams::new());
    let mut proc = PeakLimiterProcessor::new(2, 48_000, Arc::clone(&params));
    assert_eq!(proc.limiter.mode(), LimiterMode::TruePeak);

    // Control thread switches mode; the snapshot is applied on the next
    // process() sync.
    params.set_mode(LimiterMode::SamplePeak);
    let mut buffer = vec![0.25; 256 * 2];
    proc.process(&mut buffer, 2);
    assert_eq!(proc.limiter.mode(), LimiterMode::SamplePeak);

    params.set_mode(LimiterMode::TruePeak);
    proc.process(&mut buffer, 2);
    assert_eq!(proc.limiter.mode(), LimiterMode::TruePeak);
}

#[test]
fn peak_limiter_processor_mode_switch_is_allocation_free_in_process() {
    let params = Arc::new(AtomicPeakLimiterParams::new());
    let mut proc = PeakLimiterProcessor::new(2, 48_000, Arc::clone(&params));
    let mut buffer = vec![0.3; 256 * 2];
    // Warm up the cached generation so the first asserted block is steady.
    proc.process(&mut buffer, 2);

    // Flipping the atomic mode is a control-plane call (its rcu publish
    // allocates a fresh snapshot), so it stays outside the no-alloc guard.
    // Consuming the flip and processing on the audio side must not
    // allocate: the limiter switches in place.
    for i in 0..200 {
        let mode = if i % 2 == 0 {
            LimiterMode::SamplePeak
        } else {
            LimiterMode::TruePeak
        };
        params.set_mode(mode);
        assert_no_alloc::assert_no_alloc(|| {
            proc.process(&mut buffer, 2);
        });
    }
}

#[test]
fn peak_limiter_processor_disabled_bypasses() {
    let params = Arc::new(AtomicPeakLimiterParams::new());
    let mut proc = PeakLimiterProcessor::new(2, 48_000, Arc::clone(&params));

    params.set_enabled(false);
    let mut buffer = vec![1.5; 256 * 2];
    let original = buffer.clone();
    let result = proc.process(&mut buffer, 2);

    assert!(result.is_bypassed());
    assert_eq!(buffer, original);
}

#[test]
fn dynamic_loudness_sample_rate_change_preserves_published_controls() {
    let params = Arc::new(AtomicDynamicLoudnessParams::new());
    params.set_ref_volume_db(-30.0);
    params.set_strength(0.37);
    let telemetry = Arc::new(AtomicDynamicLoudnessTelemetry::new());
    let mut proc = DynamicLoudnessProcessor::new(2, 48_000, params, telemetry);
    let factor = proc.dynamic_loudness.loudness_factor();

    proc.set_sample_rate(96_000).unwrap();

    assert_eq!(proc.sample_rate, 96_000);
    assert_eq!(proc.dynamic_loudness.strength(), 0.37);
    assert_eq!(proc.dynamic_loudness.loudness_factor(), factor);
}

#[test]
fn peak_limiter_finish_releases_exact_algorithmic_delay() {
    let params = Arc::new(AtomicPeakLimiterParams::new());
    let mut proc = PeakLimiterProcessor::new(1, 48_000, params);
    let latency_frames = proc.limiter.delay_frames();
    let mut input = vec![0.0; 64];
    input[63] = 0.5;
    let _ = proc.process(&mut input, 1);
    assert!(input.iter().all(|sample| *sample == 0.0));
    assert_eq!(proc.latency().frames(), latency_frames);
    assert_eq!(proc.tail(), TailSpec::None);

    let mut drained = Vec::new();
    let mut scratch = vec![0.0; 37];
    loop {
        let progress = super::super::traits::finish_checked(
            &mut proc,
            AudioBlockMut::new(&mut scratch, 1).unwrap(),
        )
        .unwrap();
        drained.extend_from_slice(&scratch[..progress.produced_frames()]);
        if progress.state() == ProcessState::Finished {
            break;
        }
    }

    assert_eq!(drained.len(), latency_frames);
    assert!((drained[latency_frames - 1] - 0.5).abs() <= 1.0e-12);
    assert_eq!(
        super::super::traits::finish_checked(
            &mut proc,
            AudioBlockMut::new(&mut scratch, 1).unwrap(),
        )
        .unwrap(),
        ProcessProgress::finished(0)
    );
}

fn direct_interleaved_convolution(input: &[f64], ir: &[f64], channels: usize) -> Vec<f64> {
    let input_frames = input.len() / channels;
    let ir_frames = ir.len() / channels;
    let mut output = vec![0.0; (input_frames + ir_frames - 1) * channels];

    for input_frame in 0..input_frames {
        for tap in 0..ir_frames {
            let output_frame = input_frame + tap;
            for channel in 0..channels {
                output[output_frame * channels + channel] +=
                    input[input_frame * channels + channel] * ir[tap * channels + channel];
            }
        }
    }
    output
}

fn deterministic_convolver_input(frames: usize, channels: usize) -> Vec<f64> {
    (0..frames * channels)
        .map(|sample| ((sample * 7 + 3) % 19) as f64 * 0.03125 - 0.28125)
        .collect()
}

fn deterministic_convolver_ir(frames: usize, channels: usize) -> Vec<f64> {
    let mut ir = vec![0.0; frames * channels];
    for frame in 0..frames {
        for channel in 0..channels {
            let value = if frame == 0 {
                0.75 - channel as f64 * 0.125
            } else {
                let sign = if (frame + channel) % 2 == 0 {
                    1.0
                } else {
                    -1.0
                };
                sign * (0.2 + channel as f64 * 0.025) / (frame + 1) as f64
            };
            ir[frame * channels + channel] = value;
        }
    }
    ir
}

fn render_convolver_with_patterns(
    proc: &mut ConvolverProcessor,
    input: &[f64],
    channels: usize,
    process_chunks: &[usize],
    finish_chunks: &[usize],
    expected_ir_frames: usize,
) -> Vec<f64> {
    assert!(!process_chunks.is_empty());
    assert!(!finish_chunks.is_empty());
    assert!(process_chunks.iter().all(|frames| *frames > 0));
    assert!(finish_chunks.iter().all(|frames| *frames > 0));
    assert_eq!(proc.latency(), FrameDuration::ZERO);

    let input_frames = input.len() / channels;
    let mut output = Vec::with_capacity((input_frames + expected_ir_frames - 1) * channels);
    let mut cursor = 0;
    let mut chunk_index = 0;
    while cursor < input_frames {
        let frames = process_chunks[chunk_index % process_chunks.len()].min(input_frames - cursor);
        let sample_start = cursor * channels;
        let sample_end = (cursor + frames) * channels;
        let mut block = input[sample_start..sample_end].to_vec();
        let progress = super::super::traits::process_checked(
            proc,
            ProcessBuffers::in_place(AudioBlockMut::new(&mut block, channels).unwrap()),
        )
        .unwrap();
        assert_eq!(progress.consumed_frames(), frames);
        assert_eq!(progress.produced_frames(), frames);
        assert_eq!(progress.state(), ProcessState::NeedInput);
        output.extend_from_slice(&block);
        cursor += frames;
        chunk_index += 1;
    }

    assert_eq!(
        proc.tail(),
        TailSpec::finite(expected_ir_frames - 1, 48_000).unwrap()
    );

    let mut finish_index = 0;
    let final_produced = loop {
        let capacity_frames = finish_chunks[finish_index % finish_chunks.len()];
        let mut scratch = vec![0.0; capacity_frames * channels];
        let progress = super::super::traits::finish_checked(
            proc,
            AudioBlockMut::new(&mut scratch, channels).unwrap(),
        )
        .unwrap();
        output.extend_from_slice(&scratch[..progress.produced_frames() * channels]);
        if progress.state() == ProcessState::Finished {
            break progress.produced_frames();
        }
        assert_eq!(progress.state(), ProcessState::NeedOutput);
        assert_eq!(progress.produced_frames(), capacity_frames);
        finish_index += 1;
    };

    if expected_ir_frames > 1 {
        assert!(final_produced > 0);
    }
    let mut terminal_scratch = vec![0.0; finish_chunks[0] * channels];
    assert_eq!(
        super::super::traits::finish_checked(
            proc,
            AudioBlockMut::new(&mut terminal_scratch, channels).unwrap(),
        )
        .unwrap(),
        ProcessProgress::finished(0)
    );
    output
}

fn assert_convolver_matches_direct_oracle(input_frames: usize, ir_frames: usize, channels: usize) {
    let input = deterministic_convolver_input(input_frames, channels);
    let ir = deterministic_convolver_ir(ir_frames, channels);
    let expected = direct_interleaved_convolution(&input, &ir, channels);

    for (process_chunks, finish_chunks) in [
        (vec![input_frames], vec![ir_frames.max(1)]),
        (vec![1, 4, 2, 7, 3], vec![1, 5, 17, 257]),
    ] {
        let control = ConvolverControl::new(true);
        control.publish(FFTConvolver::new(&ir, channels));
        let mut proc = ConvolverProcessor::new(control).unwrap();
        proc.set_sample_rate(48_000).unwrap();
        let actual = render_convolver_with_patterns(
            &mut proc,
            &input,
            channels,
            &process_chunks,
            &finish_chunks,
            ir_frames,
        );

        assert_eq!(actual.len(), expected.len());
        for (sample, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
            assert!(
                (actual - expected).abs() <= 1.0e-8,
                "sample {sample} differs: actual={actual:?} expected={expected:?}"
            );
        }
    }
}

#[test]
fn convolver_process_and_finish_match_independent_direct_oracle() {
    let long_ir_frames = super::super::convolver::PARTITIONED_CONVOLUTION_IR_THRESHOLD + 1;
    assert_convolver_matches_direct_oracle(23, 1, 1);
    assert_convolver_matches_direct_oracle(29, 9, 2);
    assert_convolver_matches_direct_oracle(31, long_ir_frames, 1);
    assert_convolver_matches_direct_oracle(27, long_ir_frames, 2);
}

#[test]
fn convolver_reset_isolates_prior_process_and_partial_finish_history() {
    const CHANNELS: usize = 2;
    let ir = deterministic_convolver_ir(11, CHANNELS);
    let control = ConvolverControl::new(true);
    let generation = control.publish(FFTConvolver::new(&ir, CHANNELS));
    let mut proc = ConvolverProcessor::new(control.clone()).unwrap();
    proc.set_sample_rate(48_000).unwrap();

    let mut prior = deterministic_convolver_input(17, CHANNELS);
    let _ = super::super::traits::process_checked(
        &mut proc,
        ProcessBuffers::in_place(AudioBlockMut::new(&mut prior, CHANNELS).unwrap()),
    )
    .unwrap();
    let mut partial_tail = [0.0; 3 * CHANNELS];
    let partial = super::super::traits::finish_checked(
        &mut proc,
        AudioBlockMut::new(&mut partial_tail, CHANNELS).unwrap(),
    )
    .unwrap();
    assert_eq!(partial.state(), ProcessState::NeedOutput);

    proc.reset().unwrap();
    assert_eq!(control.status().latest_adopted_generation, generation);
    let input = deterministic_convolver_input(19, CHANNELS);
    let actual =
        render_convolver_with_patterns(&mut proc, &input, CHANNELS, &[2, 5, 1, 7], &[3, 4], 11);
    let expected = direct_interleaved_convolution(&input, &ir, CHANNELS);

    for (sample, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
        assert!(
            (actual - expected).abs() <= 1.0e-10,
            "sample {sample} leaked prior stream state: actual={actual:?} expected={expected:?}"
        );
    }
}

#[test]
fn convolver_sample_rate_only_retags_finite_tail_duration() {
    let control = ConvolverControl::new(true);
    let generation = control.publish(FFTConvolver::new(&[1.0, 0.5, 0.25], 1));
    let mut proc = ConvolverProcessor::new(control.clone()).unwrap();
    proc.set_sample_rate(48_000).unwrap();
    let mut input = [1.0];
    let _ = proc.process(&mut input, 1);

    assert_eq!(proc.latency(), FrameDuration::ZERO);
    assert_eq!(proc.tail(), TailSpec::finite(2, 48_000).unwrap());
    proc.set_sample_rate(96_000).unwrap();
    assert_eq!(proc.tail(), TailSpec::finite(2, 96_000).unwrap());
    assert_eq!(control.status().latest_adopted_generation, generation);

    control.set_enabled(false);
    assert_eq!(proc.tail(), TailSpec::None);
    assert_eq!(
        ConvolverProcessor::new(ConvolverControl::new(true))
            .unwrap()
            .tail(),
        TailSpec::None
    );
}

#[test]
fn convolver_finish_preserves_last_frame_impulse_tail() {
    let control = ConvolverControl::new(true);
    control.publish(FFTConvolver::new(&[1.0, 0.5, 0.25], 1));
    let mut proc = ConvolverProcessor::new(control).unwrap();
    proc.set_sample_rate(48_000).unwrap();

    let mut input = vec![0.0, 0.0, 0.0, 1.0];
    let _ = proc.process(&mut input, 1);
    assert!((input[3] - 1.0).abs() <= 1.0e-12);
    assert_eq!(proc.tail(), TailSpec::finite(2, 48_000).unwrap());

    let mut scratch = [0.0; 1];
    let first = super::super::traits::finish_checked(
        &mut proc,
        AudioBlockMut::new(&mut scratch, 1).unwrap(),
    )
    .unwrap();
    assert_eq!(first.state(), ProcessState::NeedOutput);
    assert!((scratch[0] - 0.5).abs() <= 1.0e-12);

    let second = super::super::traits::finish_checked(
        &mut proc,
        AudioBlockMut::new(&mut scratch, 1).unwrap(),
    )
    .unwrap();
    assert_eq!(second.state(), ProcessState::Finished);
    assert!((scratch[0] - 0.25).abs() <= 1.0e-12);
}

#[test]
fn convolver_terminal_finish_can_retire_to_control_quiescence() {
    let control = ConvolverControl::new(true);
    control.publish(FFTConvolver::new(&[1.0, 0.5], 1));
    let mut proc = ConvolverProcessor::new(control.clone()).unwrap();
    let mut input = [1.0];
    let _ = proc.process(&mut input, 1);
    let mut scratch = [0.0];
    assert_eq!(
        super::super::traits::finish_checked(
            &mut proc,
            AudioBlockMut::new(&mut scratch, 1).unwrap(),
        )
        .unwrap()
        .state(),
        ProcessState::Finished
    );

    control.publish(FFTConvolver::new(&[0.25], 1));
    control.set_enabled(false);
    for _ in 0..2 {
        assert_eq!(
            super::super::traits::finish_checked(
                &mut proc,
                AudioBlockMut::new(&mut scratch, 1).unwrap(),
            )
            .unwrap(),
            ProcessProgress::finished(0)
        );
    }
    assert!(control.status().backpressured);
    assert_eq!(control.status().pending_reclamations, 2);

    assert!(control.reclaim_retired());
    assert_eq!(
        super::super::traits::finish_checked(
            &mut proc,
            AudioBlockMut::new(&mut scratch, 1).unwrap(),
        )
        .unwrap(),
        ProcessProgress::finished(0)
    );
    assert!(control.reclaim_retired());
    assert!(control.is_quiescent());
}

#[test]
fn finite_finish_paths_are_allocation_free_after_processing() {
    let limiter_params = Arc::new(AtomicPeakLimiterParams::new());
    let mut limiter = PeakLimiterProcessor::new(1, 48_000, limiter_params);
    let mut limiter_input = vec![0.25; 64];
    let _ = limiter.process(&mut limiter_input, 1);
    let mut limiter_output = vec![0.0; limiter.limiter.delay_frames()];

    let control = ConvolverControl::new(true);
    control.publish(FFTConvolver::new(&[1.0, 0.5, 0.25], 1));
    let mut convolver = ConvolverProcessor::new(control).unwrap();
    let mut convolver_input = [1.0, 0.0];
    let _ = convolver.process(&mut convolver_input, 1);
    let mut convolver_output = [0.0; 2];

    assert_no_alloc::assert_no_alloc(|| {
        let limiter_progress = super::super::traits::finish_checked(
            &mut limiter,
            AudioBlockMut::new(&mut limiter_output, 1).unwrap(),
        )
        .unwrap();
        assert_eq!(limiter_progress.state(), ProcessState::Finished);

        let convolver_progress = super::super::traits::finish_checked(
            &mut convolver,
            AudioBlockMut::new(&mut convolver_output, 1).unwrap(),
        )
        .unwrap();
        assert_eq!(convolver_progress.state(), ProcessState::Finished);
    });
}
