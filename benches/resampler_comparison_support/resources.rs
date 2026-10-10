//! Resource measurements run independently of frequency-response rendering.
//! Each case gets a fresh process; allocator instrumentation is outside timing.

use std::collections::BTreeSet;
use std::process::Command;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};

use super::{
    adapters, drain_adapter, process_prepared, query_thread_cycles, ComparisonArgs, EngineFactory,
    PreparedInputs, RatePair, CHUNK_FRAMES, RATE_PAIRS, WARMUP_BUFFERS,
};
use crate::support::{
    allocation::{AllocationScope, AllocationSnapshot},
    generated_unix_ms, pin_current_thread, write_json, BenchEnvironment,
};

const MEMORY_INSTANCES: usize = 32;
const TIMING_TRIALS: usize = 4;

pub(super) fn run(argv: Vec<String>) -> Result<(), String> {
    let mut remaining = Vec::new();
    let mut selected = BTreeSet::new();
    let mut child_case = None;
    let mut milliseconds = 250u64;
    for arg in &argv {
        if arg == "--resources" {
            continue;
        } else if let Some(value) = arg.strip_prefix("--resource-engines=") {
            for id in value.split(',') {
                super::validate_engine_id(id)?;
                selected.insert(id.to_owned());
            }
        } else if let Some(value) = arg.strip_prefix("--resource-case=") {
            child_case = Some(value.to_owned());
        } else if let Some(value) = arg.strip_prefix("--resource-ms=") {
            milliseconds = value.parse().map_err(|_| "invalid --resource-ms")?;
            if !(50..=5_000).contains(&milliseconds) {
                return Err("--resource-ms must be in 50..=5000".into());
            }
        } else {
            remaining.push(arg.clone());
        }
    }
    let args = ComparisonArgs::parse(remaining)?;
    if args.perf.help {
        println!(
            "--resources [--resource-engines=id,id] [--resource-ms=250] \
                  [--pinned --pin-core 2] [--out report.json] \
                  [--engine-library id=absolute-path]"
        );
        return Ok(());
    }
    if args.perf.enforce || args.perf.baseline.is_some() {
        return Err("--resources reports resource costs and endpoint failures; --enforce/--baseline belong to the quality/throughput report".into());
    }
    let discovery = adapters::discover(
        args.libsamplerate_path.as_deref(),
        &args.native_library_paths,
        &args.required_engines,
    );
    for id in selected.iter().chain(&args.required_engines) {
        if !discovery
            .factories
            .iter()
            .any(|f| &f.identity().engine_id == id)
        {
            return Err(format!("resource engine unavailable: {id}"));
        }
    }
    let factories = discovery
        .factories
        .iter()
        .filter(|factory| selected.is_empty() || selected.contains(&factory.identity().engine_id));
    if let Some(key) = child_case {
        for factory in factories {
            for rate in RATE_PAIRS {
                if case_key(factory, rate) == key && factory.supports_rate(rate.id) {
                    let scheduling = if args.pinned {
                        Some(pin_current_thread(args.pin_core)?)
                    } else {
                        None
                    };
                    audio_engine_core::runtime::audio_thread_init();
                    let mut result = measure(factory, rate, Duration::from_millis(milliseconds))?;
                    result["scheduling"] = json!(scheduling);
                    println!("{result}");
                    return Ok(());
                }
            }
        }
        return Err(format!("resource case unavailable: {key}"));
    }

    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut cases = Vec::new();
    for factory in factories {
        for rate in RATE_PAIRS {
            if !factory.supports_rate(rate.id) {
                continue;
            }
            let key = case_key(factory, rate);
            let output = Command::new(&executable)
                .args(&argv)
                .arg(format!("--resource-case={key}"))
                .output()
                .map_err(|e| format!("resource child {key}: {e}"))?;
            if !output.status.success() {
                return Err(format!(
                    "resource child {key} exited {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
            let mut result: Value = serde_json::from_slice(&output.stdout)
                .map_err(|e| format!("resource child {key} report: {e}"))?;
            result["exit_code"] = json!(output.status.code());
            eprintln!("resampler_resources completed {key}");
            cases.push(result);
        }
    }
    let report = json!({
        "schema": "resampler_resources_v1",
        "generated_unix_ms": generated_unix_ms(),
        "environment": BenchEnvironment::capture(),
        "conditions": {
            "chunk_frames": CHUNK_FRAMES,
            "trials": TIMING_TRIALS,
            "minimum_trial_ms": milliseconds,
            "memory_instances": MEMORY_INSTANCES,
            "fresh_process_per_case": true,
            "allocation_instrumentation_during_timing": false,
            "native_heap_calls_instrumented": false,
            "cpu_budget_definition": "100 * thread_cpu_seconds / timed_input_audio_seconds; one logical CPU",
            "process_memory_scope": "entire child including DLLs/runtime; per-instance increments are pooled page-granular estimates",
            "quality_scope": "existing selected High/Linear recipes; this command does not rerun quality analysis"
        },
        "cases": cases,
        "unavailable": discovery.unavailable
    });
    if let Some(path) = args.perf.out.as_deref() {
        write_json(path, &report, "resampler resources")?;
    } else {
        println!("{report}");
    }
    Ok(())
}

fn case_key(factory: &EngineFactory, rate: RatePair) -> String {
    format!(
        "{}:{}:{}",
        factory.identity().engine_id,
        factory.identity().algorithm_id,
        rate.id
    )
}

fn allocation_json(snapshot: AllocationSnapshot) -> Value {
    json!({
        "allocations": snapshot.allocations,
        "deallocations": snapshot.deallocations,
        "reallocations": snapshot.reallocations,
        "retained_bytes": snapshot.live_bytes,
        "peak_live_bytes": snapshot.peak_live_bytes
    })
}

fn measure(factory: &EngineFactory, rate: RatePair, duration: Duration) -> Result<Value, String> {
    let inputs = PreparedInputs::new(rate);
    let scope = AllocationScope::start();
    let setup_start = Instant::now();
    let adapter = factory.create(rate, rate.channels, CHUNK_FRAMES)?;
    let setup_ns = setup_start.elapsed().as_nanos();
    let setup_heap = scope.finish();
    // All recipes get the same conservative caller capacity, independent of
    // their internal lookahead. Caller allocations are excluded from heap data.
    let output_frames = (CHUNK_FRAMES * rate.to_hz as usize)
        .div_ceil(rate.from_hz as usize)
        .saturating_add(32_768);
    if adapter.max_output_frames() > output_frames {
        return Err("resource caller capacity is below adapter requirement".into());
    }
    let mut output_f64 = vec![0.0; output_frames * rate.channels];
    let mut output_f32 = vec![0.0; output_frames * rate.channels];
    drop(adapter);

    let mut instances = Vec::with_capacity(MEMORY_INSTANCES);
    let memory_before = process_memory();
    let pool_scope = AllocationScope::start();
    for _ in 0..MEMORY_INSTANCES {
        instances.push(factory.create(rate, rate.channels, CHUNK_FRAMES)?);
    }
    let pool_heap = pool_scope.finish();
    let memory_created = process_memory();
    for instance in &mut instances {
        for _ in 0..WARMUP_BUFFERS {
            process_prepared(instance, &inputs, &mut output_f64, &mut output_f32, false)?;
        }
    }
    let memory_warm = process_memory();
    drop(instances);
    let memory_released = process_memory();

    let mut adapter = factory.create(rate, rate.channels, CHUNK_FRAMES)?;
    for _ in 0..WARMUP_BUFFERS {
        process_prepared(
            &mut adapter,
            &inputs,
            &mut output_f64,
            &mut output_f32,
            false,
        )?;
    }
    let hot_scope = AllocationScope::start();
    for index in 0..64 {
        process_prepared(
            &mut adapter,
            &inputs,
            &mut output_f64,
            &mut output_f32,
            index == 63,
        )?;
    }
    let (_, terminal) = drain_adapter(&mut adapter, &mut output_f64, &mut output_f32)?;
    adapter.reset()?;
    let hot_heap = hot_scope.finish();
    if !terminal {
        return Err("resource allocation pass did not finish".into());
    }

    let mut trials = Vec::with_capacity(TIMING_TRIALS);
    for trial in 0..TIMING_TRIALS {
        adapter.reset()?;
        let mut all_output_frames = 0usize;
        for _ in 0..WARMUP_BUFFERS {
            let progress = process_prepared(
                &mut adapter,
                &inputs,
                &mut output_f64,
                &mut output_f32,
                false,
            )?;
            if progress.consumed_frames != CHUNK_FRAMES {
                return Err("resource warmup did not consume a complete block".into());
            }
            all_output_frames += progress.produced_frames;
        }
        let cpu_start = thread_cpu_time();
        let cycles_start = query_thread_cycles();
        let wall_start = Instant::now();
        let mut calls = 0usize;
        loop {
            for _ in 0..256 {
                let progress = process_prepared(
                    &mut adapter,
                    &inputs,
                    &mut output_f64,
                    &mut output_f32,
                    false,
                )?;
                if progress.consumed_frames != CHUNK_FRAMES || progress.finished {
                    return Err("resource timed call returned incomplete progress".into());
                }
                all_output_frames += progress.produced_frames;
                std::hint::black_box(progress);
            }
            calls += 256;
            if wall_start.elapsed() >= duration {
                break;
            }
        }
        let wall_ns = wall_start.elapsed().as_nanos() as u64;
        let cycles = counter_delta(cycles_start, query_thread_cycles());
        let cpu = cpu_start.zip(thread_cpu_time()).and_then(|(start, end)| {
            Some(CpuTime {
                user_ns: end.user_ns.checked_sub(start.user_ns)?,
                kernel_ns: end.kernel_ns.checked_sub(start.kernel_ns)?,
            })
        });
        // libsamplerate requires end-of-input on the last nonempty call.
        // Keep that final block and the drain out of steady CPU/wall timing.
        let final_progress = process_prepared(
            &mut adapter,
            &inputs,
            &mut output_f64,
            &mut output_f32,
            true,
        )?;
        if final_progress.consumed_frames != CHUNK_FRAMES {
            return Err("resource final block was not consumed".into());
        }
        all_output_frames += final_progress.produced_frames;
        // A native endpoint failure must not erase CPU/memory observations.
        // Keep it explicit; resource reporting is not a quality acceptance.
        let (finished, finish_error) =
            match drain_adapter(&mut adapter, &mut output_f64, &mut output_f32) {
                Ok((drained, finished)) => {
                    all_output_frames += drained;
                    (finished, None)
                }
                Err(error) => (false, Some(error)),
            };
        let total_input = (WARMUP_BUFFERS + calls + 1) * CHUNK_FRAMES;
        let expected = adapter.expected_complete_output_frames(total_input);
        let complete_work_valid = finished && all_output_frames == expected;
        let samples = (calls * CHUNK_FRAMES * rate.channels) as f64;
        let audio_seconds = (calls * CHUNK_FRAMES) as f64 / f64::from(rate.from_hz);
        let cpu_ns = cpu.map(|t| t.user_ns + t.kernel_ns);
        trials.push(json!({
            "trial_index": trial,
            "process_calls": calls,
            "timed_input_frames": calls * CHUNK_FRAMES,
            "timed_input_audio_seconds": audio_seconds,
            "wall_ns": wall_ns,
            "thread_cpu": cpu,
            "thread_cycles": cycles,
            "wall_ns_per_input_sample": wall_ns as f64 / samples,
            "cpu_ns_per_input_sample": cpu_ns.map(|n| n as f64 / samples),
            "cycles_per_input_sample": cycles.map(|n| n as f64 / samples),
            "one_core_realtime_cpu_pct": cpu_ns.map(|n| 100.0 * n as f64 / 1e9 / audio_seconds),
            "measured_busy_thread_pct": cpu_ns.map(|n| 100.0 * n as f64 / wall_ns as f64),
            "complete_input_frames": total_input,
            "complete_output_frames": finish_error.is_none().then_some(all_output_frames),
            "expected_complete_output_frames": expected,
            "finished": finished,
            "complete_work_valid": complete_work_valid,
            "finish_error": finish_error
        }));
    }
    let private_increment = memory_before.zip(memory_warm).map(|(a, b)| {
        (b.private_commit_bytes as f64 - a.private_commit_bytes as f64) / MEMORY_INSTANCES as f64
    });
    let resident_increment = memory_before.zip(memory_warm).map(|(a, b)| {
        (b.working_set_bytes as f64 - a.working_set_bytes as f64) / MEMORY_INSTANCES as f64
    });
    Ok(json!({
        "case_key": case_key(factory, rate),
        "engine": factory.identity(),
        "rate_pair": rate.id,
        "from_rate_hz": rate.from_hz,
        "to_rate_hz": rate.to_hz,
        "channels": rate.channels,
        "output_capacity_frames": output_frames,
        "setup_instrumented_ns": setup_ns,
        "rust_setup_heap": allocation_json(setup_heap),
        "rust_pool_heap": allocation_json(pool_heap),
        "rust_process_finish_reset_heap": allocation_json(hot_heap),
        "memory_before_instances": memory_before,
        "memory_after_construction": memory_created,
        "memory_after_warmup": memory_warm,
        "memory_after_drop": memory_released,
        "pooled_private_commit_increment_bytes_per_instance": private_increment,
        "pooled_working_set_increment_bytes_per_instance": resident_increment,
        "trials": trials
    }))
}

fn counter_delta(start: Option<u64>, end: Option<u64>) -> Option<u64> {
    start.zip(end).and_then(|(a, b)| b.checked_sub(a))
}

#[derive(Clone, Copy, Serialize)]
struct CpuTime {
    user_ns: u64,
    kernel_ns: u64,
}

#[derive(Clone, Copy, Serialize)]
struct ProcessMemory {
    working_set_bytes: u64,
    peak_working_set_bytes: u64,
    private_commit_bytes: u64,
    peak_pagefile_bytes: u64,
    page_faults: u32,
}

pub(super) fn working_set_bytes() -> Option<u64> {
    process_memory().map(|memory| memory.working_set_bytes)
}

#[cfg(windows)]
fn thread_cpu_time() -> Option<CpuTime> {
    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    impl FileTime {
        fn nanoseconds(&self) -> u64 {
            ((u64::from(self.high) << 32) | u64::from(self.low)) * 100
        }
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentThread() -> isize;
        fn GetThreadTimes(
            thread: isize,
            creation: *mut FileTime,
            exit: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
    }
    let (mut creation, mut exit, mut kernel, mut user) = (
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
    );
    // SAFETY: valid pseudo handle and four correctly sized writable FILETIMEs.
    let ok = unsafe {
        GetThreadTimes(
            GetCurrentThread(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    };
    (ok != 0).then(|| CpuTime {
        user_ns: user.nanoseconds(),
        kernel_ns: kernel.nanoseconds(),
    })
}

#[cfg(not(windows))]
fn thread_cpu_time() -> Option<CpuTime> {
    None
}

#[cfg(windows)]
fn process_memory() -> Option<ProcessMemory> {
    #[repr(C)]
    #[derive(Default)]
    struct CountersEx {
        cb: u32,
        page_faults: u32,
        peak_working_set: usize,
        working_set: usize,
        peak_paged_pool: usize,
        paged_pool: usize,
        peak_nonpaged_pool: usize,
        nonpaged_pool: usize,
        pagefile: usize,
        peak_pagefile: usize,
        private_usage: usize,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> isize;
    }
    #[link(name = "psapi")]
    unsafe extern "system" {
        fn GetProcessMemoryInfo(process: isize, counters: *mut CountersEx, size: u32) -> i32;
    }
    let mut counters = CountersEx {
        cb: std::mem::size_of::<CountersEx>() as u32,
        ..Default::default()
    };
    // SAFETY: PROCESS_MEMORY_COUNTERS_EX layout, valid pseudo handle, exact cb.
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    (ok != 0).then_some(ProcessMemory {
        working_set_bytes: counters.working_set as u64,
        peak_working_set_bytes: counters.peak_working_set as u64,
        private_commit_bytes: counters.private_usage as u64,
        peak_pagefile_bytes: counters.peak_pagefile as u64,
        page_faults: counters.page_faults,
    })
}

#[cfg(not(windows))]
fn process_memory() -> Option<ProcessMemory> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn unavailable_or_decreasing_cpu_counters_are_not_reported_as_zero() {
        assert_eq!(super::counter_delta(None, Some(5)), None);
        assert_eq!(super::counter_delta(Some(6), Some(5)), None);
        assert_eq!(super::counter_delta(Some(5), Some(8)), Some(3));
    }
}
