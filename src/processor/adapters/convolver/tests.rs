use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Barrier};

use super::*;
use crate::processor::convolver::FFTConvolver;
use crate::processor::traits::{finish_checked, process_checked, AudioBlockMut, ProcessBuffers};

fn process_mono(
    processor: &mut ConvolverProcessor,
    samples: &mut [f64],
) -> Result<ProcessProgress, ProcessError> {
    process_checked(
        processor,
        ProcessBuffers::in_place(AudioBlockMut::new(samples, 1)?),
    )
}

#[test]
fn consumer_lease_rejects_second_direct_consumer_and_releases_on_drop() {
    let control = ConvolverControl::new(false);
    let first = ConvolverProcessor::new(control.clone()).unwrap();

    assert!(matches!(
        ConvolverProcessor::new(control.clone()),
        Err(ProcessError::ConsumerAlreadyActive {
            processor: "Convolver"
        })
    ));

    drop(first);
    assert!(ConvolverProcessor::new(control).is_ok());
}

#[test]
fn first_process_on_a_new_audio_thread_is_allocation_free() {
    let control = ConvolverControl::new(true);
    control.publish(FFTConvolver::new(&[0.5, 0.25], 1));
    let processor = ConvolverProcessor::new(control.clone()).unwrap();

    let processor = std::thread::spawn(move || {
        let mut processor = processor;
        let mut samples = [1.0; 128];
        assert_no_alloc::assert_no_alloc(|| {
            let _ = process_mono(&mut processor, &mut samples).unwrap();
        });
        assert_eq!(samples[0], 0.5);
        processor
    })
    .join()
    .unwrap();

    drop(processor);
}

#[test]
fn disable_during_partial_finish_preserves_locked_tail() {
    let control = ConvolverControl::new(true);
    control.publish(FFTConvolver::new(&[1.0, 0.5, 0.25, 0.125], 1));
    let mut processor = ConvolverProcessor::new(control.clone()).unwrap();
    let mut input = [1.0];
    let _ = process_mono(&mut processor, &mut input).unwrap();

    let mut output = [0.0];
    let first =
        finish_checked(&mut processor, AudioBlockMut::new(&mut output, 1).unwrap()).unwrap();
    assert_eq!(first.state(), ProcessState::NeedOutput);
    assert_eq!(output, [0.5]);

    control.set_enabled(false);
    let second =
        finish_checked(&mut processor, AudioBlockMut::new(&mut output, 1).unwrap()).unwrap();
    assert_eq!(second.state(), ProcessState::NeedOutput);
    assert_eq!(output, [0.25]);

    let third =
        finish_checked(&mut processor, AudioBlockMut::new(&mut output, 1).unwrap()).unwrap();
    assert_eq!(third.state(), ProcessState::Finished);
    assert_eq!(output, [0.125]);

    assert_eq!(
        finish_checked(&mut processor, AudioBlockMut::new(&mut output, 1).unwrap(),).unwrap(),
        ProcessProgress::finished(0)
    );
    assert!(control.reclaim_retired());
    assert!(control.is_quiescent());
}

#[test]
fn publication_during_idle_ack_cannot_commit_a_stale_generation() {
    let control = ConvolverControl::new(false);
    let audio_control = control.clone();
    let loaded = Arc::new(Barrier::new(2));
    let published = Arc::new(Barrier::new(2));
    let audio_loaded = Arc::clone(&loaded);
    let audio_published = Arc::clone(&published);

    let audio = std::thread::spawn(move || {
        audio_control.acknowledge_drained_with_test_hook(|| {
            audio_loaded.wait();
            audio_published.wait();
        });
    });

    loaded.wait();
    let generation = control.publish(FFTConvolver::new(&[1.0], 1));
    published.wait();
    audio.join().unwrap();

    let status = control.status();
    assert_eq!(status.latest_published_generation, generation);
    assert_ne!(status.audio_drained_generation, generation);
    assert!(!control.is_quiescent());
}

#[test]
fn quiescence_rechecks_retirement_after_generation_acknowledgement() {
    let control = ConvolverControl::new(false);
    let generation = control.publish(FFTConvolver::new(&[0.5], 1));
    let blocker_control = ConvolverControl::new(false);
    blocker_control.publish(FFTConvolver::new(&[1.0], 1));
    let blocker = blocker_control.take_published().unwrap();
    assert!(control.try_retire(blocker).is_ok());

    let mut processor = ConvolverProcessor::new(control.clone()).unwrap();
    let mut samples = [0.0; 4];
    let _ = process_mono(&mut processor, &mut samples).unwrap();
    assert!(control.reclaim_retired());

    let quiescent = control.is_quiescent_with_test_hook(|| {
        let _ = process_mono(&mut processor, &mut samples).unwrap();
    });

    assert_eq!(
        control.status().audio_drained_generation,
        generation,
        "the audio side must acknowledge the drained publication"
    );
    assert!(
        !quiescent,
        "a retirement stored after the first slot check must block teardown"
    );
    assert!(control.reclaim_retired());
    assert!(control.is_quiescent());
}

#[test]
fn terminal_finish_and_retirement_are_allocation_free_on_new_audio_thread() {
    let control = ConvolverControl::new(true);
    control.publish(FFTConvolver::new(&[1.0, 0.5, 0.25], 1));
    let mut processor = ConvolverProcessor::new(control.clone()).unwrap();
    let mut input = [1.0];
    let _ = process_mono(&mut processor, &mut input).unwrap();
    control.set_enabled(false);

    let processor = std::thread::spawn(move || {
        let mut processor = processor;
        let mut output = [0.0; 2];
        assert_no_alloc::assert_no_alloc(|| {
            let finished =
                finish_checked(&mut processor, AudioBlockMut::new(&mut output, 1).unwrap())
                    .unwrap();
            assert_eq!(finished.state(), ProcessState::Finished);
            let terminal =
                finish_checked(&mut processor, AudioBlockMut::new(&mut output, 1).unwrap())
                    .unwrap();
            assert_eq!(terminal, ProcessProgress::finished(0));
        });
        processor
    })
    .join()
    .unwrap();

    assert!(control.reclaim_retired());
    assert!(control.is_quiescent());
    drop(processor);
}

#[test]
fn concurrent_reclaim_and_audio_retirement_drop_every_kernel_off_audio() {
    const KERNELS: u64 = 64;

    let control = ConvolverControl::new(true);
    let audio_control = control.clone();
    let boundary = Arc::new(Barrier::new(2));
    let audio_boundary = Arc::clone(&boundary);
    let (command_tx, command_rx) = sync_channel::<bool>(0);
    let (ready_tx, ready_rx) = sync_channel(0);
    let (done_tx, done_rx) = sync_channel(0);
    let audio = std::thread::spawn(move || {
        ready_tx.send(std::thread::current().id()).unwrap();
        let mut processor = ConvolverProcessor::new(audio_control).unwrap();
        let mut samples = [1.0; 16];
        while command_rx.recv().unwrap() {
            audio_boundary.wait();
            let _ = process_mono(&mut processor, &mut samples).unwrap();
            done_tx.send(()).unwrap();
        }
        processor
    });

    let audio_thread_id = ready_rx.recv().unwrap();
    let dropped_on_audio = Arc::new(AtomicBool::new(false));
    let drop_count = Arc::new(AtomicU64::new(0));
    let make_probe = || control::ConvolverDropProbe {
        audio_thread_id,
        dropped_on_audio: Arc::clone(&dropped_on_audio),
        drop_count: Arc::clone(&drop_count),
    };
    let process_and_race_reclaim = || {
        command_tx.send(true).unwrap();
        boundary.wait();
        let _ = control.reclaim_retired();
        done_rx.recv().unwrap();
        let _ = control.reclaim_retired();
    };

    control.publish_with_drop_probe(FFTConvolver::new(&[1.0], 1), make_probe());
    process_and_race_reclaim();
    for generation in 1..KERNELS {
        let gain = 1.0 - generation as f64 / (KERNELS * 2) as f64;
        control.publish_with_drop_probe(FFTConvolver::new(&[gain], 1), make_probe());
        process_and_race_reclaim();
    }

    control.set_enabled(false);
    process_and_race_reclaim();
    assert!(control.is_quiescent());

    command_tx.send(false).unwrap();
    let processor = audio.join().unwrap();
    drop(processor);

    assert_eq!(drop_count.load(Ordering::Acquire), KERNELS);
    assert!(!dropped_on_audio.load(Ordering::Acquire));
}

#[test]
fn drained_generation_acknowledgement_handles_wrapping_publication() {
    let control = ConvolverControl::new(false);
    control.set_generation_state_for_test(u64::MAX, u64::MAX);
    let generation = control.publish(FFTConvolver::new(&[1.0], 1));
    assert_eq!(generation, 0);

    let mut processor = ConvolverProcessor::new(control.clone()).unwrap();
    let mut samples = [0.0; 4];
    let _ = process_mono(&mut processor, &mut samples).unwrap();
    assert!(control.reclaim_retired());

    let status = control.status();
    assert_eq!(status.latest_published_generation, 0);
    assert_eq!(status.audio_drained_generation, 0);
    assert!(control.is_quiescent());
}
