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
    let mut memory_only = false;
    let mut milliseconds = 250u64;
    for arg in &argv {
        if arg == "--resources" {
            continue;
        } else if arg == "--resource-memory-only" {
            memory_only = true;
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
            "--resources [--resource-engines=id,id] [--resource-ms=250] [--resource-memory-only] \
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
                    let mut result = measure(
                        factory,
                        rate,
                        Duration::from_millis(milliseconds),
                        memory_only,
                    )?;
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
        "schema": "resampler_resources_v2",
        "generated_unix_ms": generated_unix_ms(),
        "environment": BenchEnvironment::capture(),
        "conditions": {
            "chunk_frames": CHUNK_FRAMES,
            "trials": if memory_only { 0 } else { TIMING_TRIALS },
            "memory_only": memory_only,
            "minimum_trial_ms": milliseconds,
            "memory_instances": MEMORY_INSTANCES,
            "fresh_process_per_case": true,
            "allocation_instrumentation_during_timing": false,
            "native_heap_calls_instrumented": false,
            "process_heap_walk": "Windows HeapWalk snapshots, not allocation-call counts or temporal peaks; includes runtime/adapter heap blocks and allocator slabs; excludes direct VirtualAlloc payloads",
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

fn measure(
    factory: &EngineFactory,
    rate: RatePair,
    duration: Duration,
    memory_only: bool,
) -> Result<Value, String> {
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
    let heap_before = process_heap_snapshot();
    let pool_scope = AllocationScope::start();
    for _ in 0..MEMORY_INSTANCES {
        instances.push(factory.create(rate, rate.channels, CHUNK_FRAMES)?);
    }
    let pool_heap = pool_scope.finish();
    let memory_created = process_memory();
    let heap_created = process_heap_snapshot();
    for instance in &mut instances {
        for _ in 0..WARMUP_BUFFERS {
            process_prepared(instance, &inputs, &mut output_f64, &mut output_f32, false)?;
        }
    }
    let memory_warm = process_memory();
    let heap_warm = process_heap_snapshot();
    // Keep the pool's handle storage, which predates memory_before, alive for
    // the final snapshot too. Only the measured adapters are released here.
    instances.clear();
    let memory_released = process_memory();
    let heap_released = process_heap_snapshot();

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

    let trial_count = if memory_only { 0 } else { TIMING_TRIALS };
    let mut trials = Vec::with_capacity(trial_count);
    for trial in 0..trial_count {
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
    let heap_busy_increment = heap_before
        .zip(heap_warm)
        .map(|(a, b)| (b.busy_bytes as f64 - a.busy_bytes as f64) / MEMORY_INSTANCES as f64);
    let heap_busy_blocks_increment = heap_before
        .zip(heap_warm)
        .map(|(a, b)| (b.busy_blocks as f64 - a.busy_blocks as f64) / MEMORY_INSTANCES as f64);
    Ok(json!({
        "memory_only": memory_only,
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
        "process_heap_before_instances": heap_before,
        "process_heap_after_construction": heap_created,
        "process_heap_after_warmup": heap_warm,
        "process_heap_after_drop": heap_released,
        "pooled_private_commit_increment_bytes_per_instance": private_increment,
        "pooled_working_set_increment_bytes_per_instance": resident_increment,
        "pooled_process_heap_busy_bytes_increment_per_instance": heap_busy_increment,
        "pooled_process_heap_busy_blocks_increment_per_instance": heap_busy_blocks_increment,
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

#[derive(Clone, Copy, Serialize)]
struct ProcessHeapSnapshot {
    busy_bytes: u64,
    busy_blocks: u64,
    heap_count: u32,
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

#[cfg(windows)]
fn process_heap_snapshot() -> Option<ProcessHeapSnapshot> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetProcessHeaps(number_of_heaps: u32, heaps: *mut isize) -> u32;
    }
    // A stack buffer avoids measuring the snapshotter's own heap allocation.
    // Fail closed if it cannot hold the complete list; never publish a partial sum.
    let mut heaps = [0isize; 128];
    // SAFETY: the array is writable and holds exactly the advertised handle count.
    let count = unsafe { GetProcessHeaps(heaps.len() as u32, heaps.as_mut_ptr()) };
    if count == 0 || count as usize > heaps.len() {
        return None;
    }
    let mut snapshot = ProcessHeapSnapshot {
        busy_bytes: 0,
        busy_blocks: 0,
        heap_count: count,
    };
    for &heap in &heaps[..count as usize] {
        // SAFETY: handles belong to this process. Resource children do not
        // destroy heaps concurrently; no adapter or allocator is invoked here.
        let (bytes, blocks) = unsafe { heap_busy_snapshot(heap) }?;
        snapshot.busy_bytes = snapshot.busy_bytes.checked_add(bytes)?;
        snapshot.busy_blocks = snapshot.busy_blocks.checked_add(blocks)?;
    }
    Some(snapshot)
}

/// The heap handle must stay valid throughout this non-realtime snapshot.
#[cfg(windows)]
unsafe fn heap_busy_snapshot(heap: isize) -> Option<(u64, u64)> {
    // The Region member supplies storage/alignment for the SDK's union:
    // 16 bytes on Win32 and 24 on Win64. Only the prefix fields are read.
    #[repr(C)]
    struct HeapRegion {
        committed_bytes: u32,
        uncommitted_bytes: u32,
        first_block: *mut std::ffi::c_void,
        last_block: *mut std::ffi::c_void,
    }
    #[repr(C)]
    struct ProcessHeapEntry {
        data: *mut std::ffi::c_void,
        data_bytes: u32,
        overhead: u8,
        region_index: u8,
        flags: u16,
        trailing_union: HeapRegion,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn HeapLock(heap: isize) -> i32;
        fn HeapUnlock(heap: isize) -> i32;
        fn HeapWalk(heap: isize, entry: *mut ProcessHeapEntry) -> i32;
        fn GetLastError() -> u32;
    }

    const ERROR_NO_MORE_ITEMS: u32 = 259;
    const PROCESS_HEAP_ENTRY_BUSY: u16 = 0x0004;
    // SAFETY: the caller guarantees a live process heap. Locking prevents
    // mutation during traversal; no allocation, formatting or logging occurs
    // until after HeapUnlock, including all failure paths below.
    if unsafe { HeapLock(heap) } == 0 {
        return None;
    }
    let mut busy_bytes = 0u64;
    let mut busy_blocks = 0u64;
    let mut entry = ProcessHeapEntry {
        data: std::ptr::null_mut(),
        data_bytes: 0,
        overhead: 0,
        region_index: 0,
        flags: 0,
        trailing_union: HeapRegion {
            committed_bytes: 0,
            uncommitted_bytes: 0,
            first_block: std::ptr::null_mut(),
            last_block: std::ptr::null_mut(),
        },
    };
    let complete = loop {
        // SAFETY: entry matches PROCESS_HEAP_ENTRY and the heap is locked.
        let walked = unsafe { HeapWalk(heap, &mut entry) } != 0;
        if !walked {
            // SAFETY: query the calling thread's last-error immediately.
            break unsafe { GetLastError() } == ERROR_NO_MORE_ITEMS;
        }
        if entry.flags & PROCESS_HEAP_ENTRY_BUSY != 0 {
            busy_bytes = busy_bytes.saturating_add(u64::from(entry.data_bytes));
            busy_blocks = busy_blocks.saturating_add(1);
        }
    };
    // SAFETY: paired with the successful HeapLock, on every traversal outcome.
    let unlocked = unsafe { HeapUnlock(heap) } != 0;
    (complete && unlocked).then_some((busy_bytes, busy_blocks))
}

#[cfg(not(windows))]
fn process_heap_snapshot() -> Option<ProcessHeapSnapshot> {
    None
}

#[cfg(not(windows))]
fn process_memory() -> Option<ProcessMemory> {
    None
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    #[test]
    fn heap_snapshot_observes_native_allocation_and_release() {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn HeapCreate(options: u32, initial: usize, maximum: usize) -> isize;
            fn HeapAlloc(heap: isize, flags: u32, bytes: usize) -> *mut std::ffi::c_void;
            fn HeapFree(heap: isize, flags: u32, block: *mut std::ffi::c_void) -> i32;
            fn HeapDestroy(heap: isize) -> i32;
        }
        // SAFETY: a dedicated test heap stays live through every snapshot and
        // allocation/free; it is destroyed before assertions can unwind.
        let (before, during, after, freed, destroyed, observed_block) = unsafe {
            let heap = HeapCreate(0, 0, 0);
            assert_ne!(heap, 0);
            let before = super::heap_busy_snapshot(heap);
            let block = HeapAlloc(heap, 0, 12_345);
            let during = super::heap_busy_snapshot(heap);
            let freed = !block.is_null() && HeapFree(heap, 0, block) != 0;
            let after = super::heap_busy_snapshot(heap);
            let destroyed = HeapDestroy(heap) != 0;
            (before, during, after, freed, destroyed, !block.is_null())
        };
        assert!(observed_block && freed && destroyed);
        let before = before.expect("initial heap traversal");
        let during = during.expect("allocated heap traversal");
        assert!(during.0 >= before.0 + 12_345);
        assert_eq!(during.1, before.1 + 1);
        assert_eq!(after, Some(before));
        assert!(super::process_heap_snapshot().is_some());
    }

    #[test]
    fn unavailable_or_decreasing_cpu_counters_are_not_reported_as_zero() {
        assert_eq!(super::counter_delta(None, Some(5)), None);
        assert_eq!(super::counter_delta(Some(6), Some(5)), None);
        assert_eq!(super::counter_delta(Some(5), Some(8)), Some(3));
    }
}
