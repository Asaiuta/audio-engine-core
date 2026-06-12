use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use audio_engine_core::processor::{
    AtomicCrossfeedParams, AtomicDynamicLoudnessParams, AtomicEqParams, AtomicNoiseShaperParams,
    AtomicPeakLimiterParams, AtomicSaturationParams, AtomicVolumeParams, CrossfeedParamsSnapshot,
    DynamicLoudnessParamsSnapshot, EqParamsSnapshot, NoiseShaperParamsSnapshot,
    PeakLimiterParamsSnapshot, SaturationParamsSnapshot, VolumeParamsSnapshot, EQ_BANDS,
};

const LOUDNESS_BANDS: usize = 7;

fn main() {
    let args = std::env::args().collect::<Vec<_>>();
    let quick = args.iter().any(|arg| arg == "--quick");
    let enforce = args.iter().any(|arg| arg == "--enforce");
    let iterations = if quick { 250_000 } else { 2_000_000 };
    let update_interval = if quick { 2_048 } else { 8_192 };

    println!(
        "audio_lockfree_params_perf iterations={iterations} update_interval={update_interval}"
    );

    let steady = benchmark_steady_state(iterations);
    print_report("lockfree_params_generation_steady_state", &steady);

    let occasional = benchmark_occasional_update(iterations, update_interval);
    print_report("lockfree_params_generation_occasional_update", &occasional);

    let arc_guard = benchmark_arc_guard_steady_state(iterations);
    print_pair_report("lockfree_params_arc_guard_steady_state", &arc_guard);

    if enforce {
        assert!(
            steady.improvement_percent >= 3.0,
            "steady-state lockfree param read improvement below 3%: {:.2}%",
            steady.improvement_percent
        );
    }
}

struct BenchReport {
    current_ns_per_read: f64,
    legacy_ns_per_read: f64,
    improvement_percent: f64,
}

struct CurrentParams {
    eq: Arc<AtomicEqParams>,
    saturation: Arc<AtomicSaturationParams>,
    crossfeed: Arc<AtomicCrossfeedParams>,
    limiter: Arc<AtomicPeakLimiterParams>,
    volume: Arc<AtomicVolumeParams>,
    noise: Arc<AtomicNoiseShaperParams>,
    dynamic: Arc<AtomicDynamicLoudnessParams>,
}

struct CurrentCache {
    eq: Arc<EqParamsSnapshot>,
    saturation: Arc<SaturationParamsSnapshot>,
    crossfeed: Arc<CrossfeedParamsSnapshot>,
    limiter: Arc<PeakLimiterParamsSnapshot>,
    volume: Arc<VolumeParamsSnapshot>,
    noise: Arc<NoiseShaperParamsSnapshot>,
    dynamic: Arc<DynamicLoudnessParamsSnapshot>,
    generation_eq: u64,
    generation_saturation: u64,
    generation_crossfeed: u64,
    generation_limiter: u64,
    generation_volume: u64,
    generation_noise: u64,
    generation_dynamic: u64,
}

impl CurrentParams {
    fn new() -> Self {
        let eq = Arc::new(AtomicEqParams::new());
        let saturation = Arc::new(AtomicSaturationParams::new());
        let crossfeed = Arc::new(AtomicCrossfeedParams::new());
        let limiter = Arc::new(AtomicPeakLimiterParams::new());
        let volume = Arc::new(AtomicVolumeParams::new());
        let noise = Arc::new(AtomicNoiseShaperParams::new());
        let dynamic = Arc::new(AtomicDynamicLoudnessParams::new());

        eq.set_enabled(true);
        saturation.set_enabled(true);
        crossfeed.set_enabled(true);
        limiter.set_enabled(true);
        noise.set_enabled(true);
        dynamic.set_enabled(true);

        Self {
            eq,
            saturation,
            crossfeed,
            limiter,
            volume,
            noise,
            dynamic,
        }
    }

    fn cache(&self) -> CurrentCache {
        let (eq, generation_eq) = self.eq.load_with_generation();
        let (saturation, generation_saturation) = self.saturation.load_with_generation();
        let (crossfeed, generation_crossfeed) = self.crossfeed.load_with_generation();
        let (limiter, generation_limiter) = self.limiter.load_with_generation();
        let (volume, generation_volume) = self.volume.load_with_generation();
        let (noise, generation_noise) = self.noise.load_with_generation();
        let (dynamic, generation_dynamic) = self.dynamic.load_with_generation();
        CurrentCache {
            eq,
            saturation,
            crossfeed,
            limiter,
            volume,
            noise,
            dynamic,
            generation_eq,
            generation_saturation,
            generation_crossfeed,
            generation_limiter,
            generation_volume,
            generation_noise,
            generation_dynamic,
        }
    }

    fn publish_update(&self, i: usize) {
        let gain = ((i % 31) as f64 - 15.0) * 0.25;
        self.eq.set_band_gain(i % EQ_BANDS, gain);
        self.saturation.set_drive(0.25 + (i % 16) as f64 * 0.01);
        self.crossfeed.set_mix(0.2 + (i % 10) as f64 * 0.03);
        self.limiter.set_threshold(-3.0 + (i % 6) as f64 * 0.25);
        self.volume.set_volume(0.5 + (i % 20) as f64 * 0.01);
        self.noise.set_bits(16 + (i % 9) as u32);
        self.dynamic.set_strength(0.5 + (i % 20) as f64 * 0.01);
    }

    fn sync_cached(&self, cache: &mut CurrentCache) -> f64 {
        if let Some((next, generation)) = self.eq.load_if_changed_since(cache.generation_eq) {
            cache.eq = next;
            cache.generation_eq = generation;
        }
        if let Some((next, generation)) = self
            .saturation
            .load_if_changed_since(cache.generation_saturation)
        {
            cache.saturation = next;
            cache.generation_saturation = generation;
        }
        if let Some((next, generation)) = self
            .crossfeed
            .load_if_changed_since(cache.generation_crossfeed)
        {
            cache.crossfeed = next;
            cache.generation_crossfeed = generation;
        }
        if let Some((next, generation)) =
            self.limiter.load_if_changed_since(cache.generation_limiter)
        {
            cache.limiter = next;
            cache.generation_limiter = generation;
        }
        if let Some((next, generation)) = self.volume.load_if_changed_since(cache.generation_volume)
        {
            cache.volume = next;
            cache.generation_volume = generation;
        }
        if let Some((next, generation)) = self.noise.load_if_changed_since(cache.generation_noise) {
            cache.noise = next;
            cache.generation_noise = generation;
        }
        if let Some((next, generation)) =
            self.dynamic.load_if_changed_since(cache.generation_dynamic)
        {
            cache.dynamic = next;
            cache.generation_dynamic = generation;
        }

        current_cache_sum(cache)
    }

    fn sync_cached_arc_guard(&self, cache: &mut CurrentCache) -> f64 {
        if let Some(next) = self.eq.load_if_changed(&cache.eq) {
            cache.eq = next;
        }
        if let Some(next) = self.saturation.load_if_changed(&cache.saturation) {
            cache.saturation = next;
        }
        if let Some(next) = self.crossfeed.load_if_changed(&cache.crossfeed) {
            cache.crossfeed = next;
        }
        if let Some(next) = self.limiter.load_if_changed(&cache.limiter) {
            cache.limiter = next;
        }
        if let Some(next) = self.volume.load_if_changed(&cache.volume) {
            cache.volume = next;
        }
        if let Some(next) = self.noise.load_if_changed(&cache.noise) {
            cache.noise = next;
        }
        if let Some(next) = self.dynamic.load_if_changed(&cache.dynamic) {
            cache.dynamic = next;
        }

        current_cache_sum(cache)
    }
}

fn current_cache_sum(cache: &CurrentCache) -> f64 {
    let eq_sum: f64 = cache.eq.gains.iter().sum();
    eq_sum
        + cache.saturation.drive
        + cache.saturation.threshold
        + cache.saturation.mix
        + cache.saturation.input_gain_db
        + cache.saturation.output_gain_db
        + cache.saturation.highpass_cutoff
        + cache.crossfeed.mix
        + cache.crossfeed.cutoff_hz
        + cache.limiter.threshold_db
        + cache.limiter.release_ms
        + cache.volume.volume
        + cache.noise.bits as f64
        + cache.dynamic.volume
        + cache.dynamic.strength
        + bool_value(cache.eq.enabled)
        + bool_value(cache.saturation.enabled)
        + bool_value(cache.crossfeed.enabled)
        + bool_value(cache.limiter.enabled)
        + bool_value(cache.volume.muted)
        + bool_value(cache.noise.enabled)
        + bool_value(cache.dynamic.enabled)
}

struct LegacyParams {
    eq_generation: AtomicU64,
    eq_observed_generation: AtomicU64,
    eq_gains: [AtomicU64; EQ_BANDS],
    eq_enabled: AtomicBool,
    saturation_drive: AtomicU64,
    saturation_threshold: AtomicU64,
    saturation_mix: AtomicU64,
    saturation_type: AtomicU64,
    saturation_input_gain: AtomicU64,
    saturation_output_gain: AtomicU64,
    saturation_highpass_mode: AtomicBool,
    saturation_highpass_cutoff: AtomicU64,
    saturation_enabled: AtomicBool,
    crossfeed_mix: AtomicU64,
    crossfeed_cutoff: AtomicU64,
    crossfeed_enabled: AtomicBool,
    limiter_threshold: AtomicU64,
    limiter_release: AtomicU64,
    limiter_enabled: AtomicBool,
    volume: AtomicU64,
    muted: AtomicBool,
    noise_enabled: AtomicBool,
    noise_bits: AtomicU64,
    noise_curve: AtomicU64,
    dynamic_enabled: AtomicBool,
    dynamic_volume: AtomicU64,
    dynamic_strength: AtomicU64,
    telemetry_factor: AtomicU64,
    telemetry_band_gains: [AtomicU64; LOUDNESS_BANDS],
}

impl LegacyParams {
    #[inline(never)]
    fn opaque_f64(value: &AtomicU64) -> f64 {
        f64::from_bits(black_box(value).load(Ordering::Acquire))
    }

    #[inline(never)]
    fn opaque_u64(value: &AtomicU64) -> u64 {
        black_box(value).load(Ordering::Acquire)
    }

    #[inline(never)]
    fn opaque_bool(value: &AtomicBool) -> bool {
        black_box(value).load(Ordering::Acquire)
    }

    fn new() -> Self {
        Self {
            eq_generation: AtomicU64::new(1),
            eq_observed_generation: AtomicU64::new(0),
            eq_gains: std::array::from_fn(|_| AtomicU64::new(0.0_f64.to_bits())),
            eq_enabled: AtomicBool::new(true),
            saturation_drive: AtomicU64::new(0.25_f64.to_bits()),
            saturation_threshold: AtomicU64::new(0.88_f64.to_bits()),
            saturation_mix: AtomicU64::new(0.2_f64.to_bits()),
            saturation_type: AtomicU64::new(0),
            saturation_input_gain: AtomicU64::new(0.0_f64.to_bits()),
            saturation_output_gain: AtomicU64::new(0.0_f64.to_bits()),
            saturation_highpass_mode: AtomicBool::new(false),
            saturation_highpass_cutoff: AtomicU64::new(4000.0_f64.to_bits()),
            saturation_enabled: AtomicBool::new(true),
            crossfeed_mix: AtomicU64::new(0.35_f64.to_bits()),
            crossfeed_cutoff: AtomicU64::new(700.0_f64.to_bits()),
            crossfeed_enabled: AtomicBool::new(true),
            limiter_threshold: AtomicU64::new((-1.0_f64).to_bits()),
            limiter_release: AtomicU64::new(150.0_f64.to_bits()),
            limiter_enabled: AtomicBool::new(true),
            volume: AtomicU64::new(1.0_f64.to_bits()),
            muted: AtomicBool::new(false),
            noise_enabled: AtomicBool::new(true),
            noise_bits: AtomicU64::new(24),
            noise_curve: AtomicU64::new(0),
            dynamic_enabled: AtomicBool::new(true),
            dynamic_volume: AtomicU64::new(1.0_f64.to_bits()),
            dynamic_strength: AtomicU64::new(1.0_f64.to_bits()),
            telemetry_factor: AtomicU64::new(0.0_f64.to_bits()),
            telemetry_band_gains: std::array::from_fn(|_| AtomicU64::new(0.0_f64.to_bits())),
        }
    }

    fn publish_update(&self, i: usize) {
        let gain = ((i % 31) as f64 - 15.0) * 0.25;
        self.eq_gains[i % EQ_BANDS].store(gain.to_bits(), Ordering::Release);
        self.eq_generation.fetch_add(1, Ordering::Release);
        self.saturation_drive
            .store((0.25 + (i % 16) as f64 * 0.01).to_bits(), Ordering::Release);
        self.crossfeed_mix
            .store((0.2 + (i % 10) as f64 * 0.03).to_bits(), Ordering::Release);
        self.limiter_threshold
            .store((-3.0 + (i % 6) as f64 * 0.25).to_bits(), Ordering::Release);
        self.volume
            .store((0.5 + (i % 20) as f64 * 0.01).to_bits(), Ordering::Release);
        self.noise_bits
            .store((16 + (i % 9)) as u64, Ordering::Release);
        self.dynamic_strength
            .store((0.5 + (i % 20) as f64 * 0.01).to_bits(), Ordering::Release);
    }

    #[inline(never)]
    fn read_all(&self) -> f64 {
        let start_version = Self::opaque_u64(&self.eq_generation);
        let eq_sum: f64 = self.eq_gains.iter().map(Self::opaque_f64).sum();
        let eq_enabled = Self::opaque_bool(&self.eq_enabled);
        let end_version = Self::opaque_u64(&self.eq_generation);
        self.eq_observed_generation
            .store(end_version.max(start_version), Ordering::Release);

        eq_sum
            + Self::opaque_f64(&self.saturation_drive)
            + Self::opaque_f64(&self.saturation_threshold)
            + Self::opaque_f64(&self.saturation_mix)
            + Self::opaque_u64(&self.saturation_type) as f64
            + Self::opaque_f64(&self.saturation_input_gain)
            + Self::opaque_f64(&self.saturation_output_gain)
            + Self::opaque_f64(&self.saturation_highpass_cutoff)
            + Self::opaque_f64(&self.crossfeed_mix)
            + Self::opaque_f64(&self.crossfeed_cutoff)
            + Self::opaque_f64(&self.limiter_threshold)
            + Self::opaque_f64(&self.limiter_release)
            + Self::opaque_f64(&self.volume)
            + Self::opaque_u64(&self.noise_bits) as f64
            + Self::opaque_u64(&self.noise_curve) as f64
            + Self::opaque_f64(&self.dynamic_volume)
            + Self::opaque_f64(&self.dynamic_strength)
            + Self::opaque_f64(&self.telemetry_factor)
            + self
                .telemetry_band_gains
                .iter()
                .map(Self::opaque_f64)
                .sum::<f64>()
            + bool_value(eq_enabled)
            + bool_value(Self::opaque_bool(&self.saturation_highpass_mode))
            + bool_value(Self::opaque_bool(&self.saturation_enabled))
            + bool_value(Self::opaque_bool(&self.crossfeed_enabled))
            + bool_value(Self::opaque_bool(&self.limiter_enabled))
            + bool_value(Self::opaque_bool(&self.muted))
            + bool_value(Self::opaque_bool(&self.noise_enabled))
            + bool_value(Self::opaque_bool(&self.dynamic_enabled))
    }
}

fn benchmark_steady_state(iterations: usize) -> BenchReport {
    let current = CurrentParams::new();
    let mut current_cache = current.cache();
    let legacy = LegacyParams::new();

    let current_duration = measure(
        || {
            let mut sum = 0.0;
            for _ in 0..iterations {
                sum += current.sync_cached(black_box(&mut current_cache));
            }
            black_box(sum)
        },
        1,
    );

    let legacy_duration = measure(
        || {
            let mut sum = 0.0;
            for _ in 0..iterations {
                sum += legacy.read_all();
            }
            black_box(sum)
        },
        1,
    );

    report(current_duration, legacy_duration, iterations)
}

fn benchmark_arc_guard_steady_state(iterations: usize) -> PairReport {
    let current = CurrentParams::new();
    let mut generation_cache = current.cache();
    let mut guard_cache = current.cache();

    let generation_duration = measure(
        || {
            let mut sum = 0.0;
            for _ in 0..iterations {
                sum += current.sync_cached(black_box(&mut generation_cache));
            }
            black_box(sum)
        },
        1,
    );

    let guard_duration = measure(
        || {
            let mut sum = 0.0;
            for _ in 0..iterations {
                sum += current.sync_cached_arc_guard(black_box(&mut guard_cache));
            }
            black_box(sum)
        },
        1,
    );

    let generation_ns = nanos_per_unit(generation_duration, iterations);
    let guard_ns = nanos_per_unit(guard_duration, iterations);
    PairReport {
        current_ns_per_read: generation_ns,
        baseline_ns_per_read: guard_ns,
        improvement_percent: (guard_ns - generation_ns) / guard_ns * 100.0,
    }
}

fn benchmark_occasional_update(iterations: usize, update_interval: usize) -> BenchReport {
    let current = CurrentParams::new();
    let mut current_cache = current.cache();
    let legacy = LegacyParams::new();

    let current_duration = measure(
        || {
            let mut sum = 0.0;
            for i in 0..iterations {
                if i % update_interval == 0 {
                    current.publish_update(i);
                }
                sum += current.sync_cached(black_box(&mut current_cache));
            }
            black_box(sum)
        },
        1,
    );

    let legacy_duration = measure(
        || {
            let mut sum = 0.0;
            for i in 0..iterations {
                if i % update_interval == 0 {
                    legacy.publish_update(i);
                }
                sum += legacy.read_all();
            }
            black_box(sum)
        },
        1,
    );

    report(current_duration, legacy_duration, iterations)
}

struct PairReport {
    current_ns_per_read: f64,
    baseline_ns_per_read: f64,
    improvement_percent: f64,
}

fn measure<T>(mut run: impl FnMut() -> T, iterations: usize) -> Duration {
    let start = Instant::now();
    for _ in 0..iterations {
        black_box(run());
    }
    start.elapsed()
}

fn report(current: Duration, legacy: Duration, reads: usize) -> BenchReport {
    let current_ns_per_read = nanos_per_unit(current, reads);
    let legacy_ns_per_read = nanos_per_unit(legacy, reads);
    BenchReport {
        current_ns_per_read,
        legacy_ns_per_read,
        improvement_percent: (legacy_ns_per_read - current_ns_per_read) / legacy_ns_per_read
            * 100.0,
    }
}

fn nanos_per_unit(duration: Duration, units: usize) -> f64 {
    duration.as_secs_f64() * 1_000_000_000.0 / units as f64
}

fn bool_value(value: bool) -> f64 {
    if value {
        1.0
    } else {
        0.0
    }
}

fn print_report(name: &str, report: &BenchReport) {
    println!(
        "{name} current={:.3} ns/read legacy_split_atomic={:.3} ns/read improvement={:.2}%",
        report.current_ns_per_read, report.legacy_ns_per_read, report.improvement_percent
    );
}

fn print_pair_report(name: &str, report: &PairReport) {
    println!(
        "{name} generation={:.3} ns/read arc_guard={:.3} ns/read improvement={:.2}%",
        report.current_ns_per_read, report.baseline_ns_per_read, report.improvement_percent
    );
}
