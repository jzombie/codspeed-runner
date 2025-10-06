use crate::prelude::*;
use runner_shared::{fifo::MarkerType, metadata::PerfMetadata};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

const NANOSECONDS_IN_SECOND: f64 = 1_000_000_000.0;
const IQR_OUTLIER_FACTOR: f64 = 1.5;
const STDEV_OUTLIER_FACTOR: f64 = 3.0;

/// Ingest Criterion results from `criterion_dir` and write CodSpeed-friendly artifacts into `profile_folder`.
pub fn ingest_criterion_results(criterion_dir: &Path, profile_folder: &Path) -> Result<()> {
    if !criterion_dir.exists() {
        bail!(
            "Criterion directory does not exist: {}",
            criterion_dir.display()
        );
    }

    let mut benchmark_dirs = Vec::new();
    collect_benchmark_dirs(criterion_dir, &mut benchmark_dirs)?;

    if benchmark_dirs.is_empty() {
        bail!(
            "No Criterion benchmarks found under {}",
            criterion_dir.display()
        );
    }

    let mut benchmarks = Vec::new();
    let mut skipped = 0usize;
    for bench_dir in benchmark_dirs {
        match build_walltime_benchmark(criterion_dir, &bench_dir) {
            Ok(Some(bench)) => benchmarks.push(bench),
            Ok(None) => skipped += 1,
            Err(err) => {
                skipped += 1;
                debug!(
                    "Skipping benchmark at {} due to error: {err:?}",
                    bench_dir.display()
                );
            }
        }
    }

    if benchmarks.is_empty() {
        bail!(
            "Failed to ingest any Criterion benchmarks from {} (skipped {skipped})",
            criterion_dir.display()
        );
    }

    // Stable ordering for deterministic outputs
    benchmarks.sort_by(|a, b| a.name().cmp(b.name()));

    fs::create_dir_all(profile_folder)?;

    let results = WalltimeResults::new(benchmarks.clone());

    let results_dir = profile_folder.join("results");
    fs::create_dir_all(&results_dir).context("Failed to create results directory")?;
    let results_json_path = results_dir.join(format!("{}.json", std::process::id()));
    let results_json_file =
        std::fs::File::create(&results_json_path).context("Failed to create results JSON file")?;
    serde_json::to_writer_pretty(&results_json_file, &results)
        .context("Failed to write results JSON file")?;

    let bench_json_path = profile_folder.join("codspeed-benchmarks.json");
    let bench_json_file = std::fs::File::create(&bench_json_path)
        .context("Failed to create codspeed-benchmarks.json")?;
    serde_json::to_writer_pretty(bench_json_file, &results)
        .context("Failed to write codspeed-benchmarks.json")?;

    let mut uri_by_ts = Vec::with_capacity(benchmarks.len());
    let mut markers = Vec::with_capacity(benchmarks.len() * 2);
    let mut timeline_cursor = 0u64;

    for bench in &benchmarks {
        uri_by_ts.push((timeline_cursor, bench.uri().to_string()));
        markers.push(MarkerType::SampleStart(timeline_cursor));

        let raw_duration_ns = (bench.stats.total_time * NANOSECONDS_IN_SECOND).round();
        let duration_ns = raw_duration_ns
            .is_finite()
            .then_some(raw_duration_ns.max(1.0))
            .unwrap_or(1.0) as u64;
        let end_ts = timeline_cursor + duration_ns;

        markers.push(MarkerType::SampleEnd(end_ts));
        timeline_cursor = end_ts.saturating_add(1);
    }

    let metadata = PerfMetadata {
        version: 1,
        integration: ("codspeed-runner".into(), env!("CARGO_PKG_VERSION").into()),
        uri_by_ts,
        ignored_modules: vec![],
        markers,
    };
    metadata
        .save_to(profile_folder)
        .context("Failed to write perf.metadata")?;

    Ok(())
}

fn collect_benchmark_dirs(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let new_dir = path.join("new");
        if new_dir.join("sample.json").exists()
            || new_dir.join("estimates.json").exists()
            || new_dir.join("raw.csv").exists()
        {
            out.push(path);
            continue;
        }

        collect_benchmark_dirs(&path, out)?;
    }
    Ok(())
}

fn build_walltime_benchmark(
    criterion_root: &Path,
    bench_dir: &Path,
) -> Result<Option<WalltimeBenchmark>> {
    let measurements = match load_benchmark_measurements(bench_dir) {
        Some(data) => data,
        None => return Ok(None),
    };

    let identity = determine_identity(criterion_root, bench_dir);
    let stats = BenchmarkStats::from_measurements(&measurements);

    if stats.mean_ns < f64::EPSILON {
        return Ok(None);
    }

    let benchmark = WalltimeBenchmark {
        metadata: BenchmarkMetadata {
            name: identity.name,
            uri: identity.uri,
        },
        config: BenchmarkConfig {
            warmup_time_ns: None,
            min_round_time_ns: None,
            max_time_ns: measurements.max_time_ns,
            max_rounds: None,
        },
        stats,
    };

    Ok(Some(benchmark))
}

fn load_benchmark_measurements(dir: &Path) -> Option<BenchmarkMeasurements> {
    let new_dir = dir.join("new");

    if let Ok(sample) = fs::read_to_string(new_dir.join("sample.json")) {
        if let Ok(sample) = serde_json::from_str::<SavedSample>(&sample) {
            if let Some(measurements) = BenchmarkMeasurements::from_sample(sample) {
                return Some(measurements);
            }
        }
    }

    if let Ok(estimates) = fs::read_to_string(new_dir.join("estimates.json")) {
        if let Ok(estimates) = serde_json::from_str::<EstimatesRoot>(&estimates) {
            if let Some(measurements) = BenchmarkMeasurements::from_estimates(estimates) {
                return Some(measurements);
            }
        }
    }

    None
}

fn determine_identity(criterion_root: &Path, dir: &Path) -> BenchmarkIdentity {
    let new_dir = dir.join("new");
    if let Ok(benchmark_id) = fs::read_to_string(new_dir.join("benchmark.json")) {
        if let Ok(id) = serde_json::from_str::<BenchmarkIdRecord>(&benchmark_id) {
            let mut name = id.group_id.clone();
            if let Some(function) = id.function_id {
                if !function.is_empty() {
                    name.push_str("::");
                    name.push_str(&function);
                }
            }
            if let Some(parameter) = id.value_str {
                if !parameter.is_empty() {
                    name.push_str(&format!("[{parameter}]"));
                }
            }
            let uri = format!("criterion::{name}");
            return BenchmarkIdentity { name, uri };
        }
    }

    let relative = dir
        .strip_prefix(criterion_root)
        .unwrap_or(dir)
        .iter()
        .map(|component| component.to_string_lossy())
        .collect::<Vec<_>>()
        .join("::");
    let name = if relative.is_empty() {
        dir.file_name()
            .map(|os| os.to_string_lossy().to_string())
            .unwrap_or_else(|| "benchmark".to_string())
    } else {
        relative.clone()
    };
    let uri_suffix = if relative.is_empty() {
        name.clone()
    } else {
        relative
    };
    let uri = format!("criterion::{uri_suffix}");

    BenchmarkIdentity { name, uri }
}

#[derive(Clone, Debug, Serialize)]
struct WalltimeResults {
    creator: Creator,
    instrument: Instrument,
    benchmarks: Vec<WalltimeBenchmark>,
}

impl WalltimeResults {
    fn new(benchmarks: Vec<WalltimeBenchmark>) -> Self {
        Self {
            creator: Creator {
                name: "codspeed-rust".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                pid: std::process::id(),
            },
            instrument: Instrument {
                type_: "walltime".to_string(),
            },
            benchmarks,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
struct Creator {
    name: String,
    version: String,
    pid: u32,
}

#[derive(Clone, Debug, Serialize)]
struct Instrument {
    #[serde(rename = "type")]
    type_: String,
}

#[derive(Clone, Debug, Serialize)]
struct WalltimeBenchmark {
    #[serde(flatten)]
    metadata: BenchmarkMetadata,
    config: BenchmarkConfig,
    stats: BenchmarkStats,
}

impl WalltimeBenchmark {
    fn name(&self) -> &str {
        &self.metadata.name
    }

    fn uri(&self) -> &str {
        &self.metadata.uri
    }
}

#[derive(Clone, Debug, Serialize)]
struct BenchmarkMetadata {
    name: String,
    uri: String,
}

#[derive(Clone, Debug, Serialize)]
struct BenchmarkConfig {
    warmup_time_ns: Option<f64>,
    min_round_time_ns: Option<f64>,
    max_time_ns: Option<f64>,
    max_rounds: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
struct BenchmarkStats {
    min_ns: f64,
    max_ns: f64,
    mean_ns: f64,
    stdev_ns: f64,
    q1_ns: f64,
    median_ns: f64,
    q3_ns: f64,
    rounds: u64,
    total_time: f64,
    iqr_outlier_rounds: u64,
    stdev_outlier_rounds: u64,
    iter_per_round: u64,
    warmup_iters: u64,
}

impl BenchmarkStats {
    fn from_measurements(measurements: &BenchmarkMeasurements) -> Self {
        let rounds = measurements.per_iter_ns.len() as u64;

        let min_ns = measurements
            .per_iter_ns
            .iter()
            .copied()
            .fold(f64::INFINITY, f64::min);
        let max_ns = measurements
            .per_iter_ns
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);

        let mean_ns = measurements.mean();
        let stdev_ns = measurements.stdev();

        let (q1_ns, median_ns, q3_ns) = measurements.quantiles();
        let (iqr_outlier_rounds, stdev_outlier_rounds) =
            measurements.outlier_counts(mean_ns, stdev_ns, q1_ns, q3_ns);

        Self {
            min_ns: if !min_ns.is_finite() { mean_ns } else { min_ns },
            max_ns: if !max_ns.is_finite() { mean_ns } else { max_ns },
            mean_ns,
            stdev_ns,
            q1_ns,
            median_ns,
            q3_ns,
            rounds,
            total_time: measurements.total_time,
            iqr_outlier_rounds,
            stdev_outlier_rounds,
            iter_per_round: measurements.iter_per_round,
            warmup_iters: measurements.warmup_iters,
        }
    }
}

#[derive(Clone, Debug)]
struct BenchmarkMeasurements {
    per_iter_ns: Vec<f64>,
    total_time: f64,
    iter_per_round: u64,
    warmup_iters: u64,
    max_time_ns: Option<f64>,
    stdev_override: Option<f64>,
}

impl BenchmarkMeasurements {
    fn from_sample(sample: SavedSample) -> Option<Self> {
        if sample.iters.is_empty()
            || sample.times.is_empty()
            || sample.iters.len() != sample.times.len()
        {
            return None;
        }

        let mut per_iter_ns = Vec::with_capacity(sample.times.len());
        let mut total_time_ns = 0f64;
        let mut iter_sum = 0f64;

        for (iters, time) in sample.iters.iter().zip(sample.times.iter()) {
            if *iters <= f64::EPSILON {
                return None;
            }
            per_iter_ns.push(time / iters);
            total_time_ns += *time;
            iter_sum += *iters;
        }

        if per_iter_ns.is_empty() {
            return None;
        }

        let iter_per_round = if iter_sum <= f64::EPSILON {
            1
        } else {
            (iter_sum / per_iter_ns.len() as f64).round() as u64
        };

        Some(Self {
            per_iter_ns,
            total_time: total_time_ns / NANOSECONDS_IN_SECOND,
            iter_per_round: iter_per_round.max(1),
            warmup_iters: 0,
            max_time_ns: None,
            stdev_override: None,
        })
    }

    fn from_estimates(estimates: EstimatesRoot) -> Option<Self> {
        let EstimatesRoot {
            mean,
            median,
            std_dev,
        } = estimates;

        let mean_estimate = mean.or(median).map(|estimate| estimate.point_estimate)?;

        if mean_estimate <= f64::EPSILON {
            return None;
        }

        let mean_ns = mean_estimate * NANOSECONDS_IN_SECOND;
        let stdev_override = std_dev
            .map(|std| std.point_estimate * NANOSECONDS_IN_SECOND)
            .filter(|v| v.is_finite() && *v >= 0.0);

        Some(Self {
            per_iter_ns: vec![mean_ns],
            total_time: mean_estimate,
            iter_per_round: 1,
            warmup_iters: 0,
            max_time_ns: None,
            stdev_override,
        })
    }

    fn mean(&self) -> f64 {
        if self.per_iter_ns.is_empty() {
            return 0.0;
        }
        self.per_iter_ns.iter().sum::<f64>() / self.per_iter_ns.len() as f64
    }

    fn stdev(&self) -> f64 {
        if let Some(override_value) = self.stdev_override {
            return override_value;
        }

        let n = self.per_iter_ns.len();
        if n < 2 {
            return 0.0;
        }
        let mean = self.mean();
        let variance = self
            .per_iter_ns
            .iter()
            .map(|value| {
                let diff = value - mean;
                diff * diff
            })
            .sum::<f64>()
            / (n as f64 - 1.0);
        variance.sqrt()
    }

    fn quantiles(&self) -> (f64, f64, f64) {
        if self.per_iter_ns.is_empty() {
            return (0.0, 0.0, 0.0);
        }

        let mut sorted = self.per_iter_ns.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let q1 = quantile(&sorted, 0.25);
        let median = quantile(&sorted, 0.50);
        let q3 = quantile(&sorted, 0.75);
        (q1, median, q3)
    }

    fn outlier_counts(&self, mean_ns: f64, stdev_ns: f64, q1_ns: f64, q3_ns: f64) -> (u64, u64) {
        if self.per_iter_ns.is_empty() {
            return (0, 0);
        }

        let iqr = q3_ns - q1_ns;
        let iqr_low = q1_ns - IQR_OUTLIER_FACTOR * iqr;
        let iqr_high = q3_ns + IQR_OUTLIER_FACTOR * iqr;
        let iqr_outlier_rounds = if iqr <= f64::EPSILON {
            0
        } else {
            self.per_iter_ns
                .iter()
                .filter(|&&value| value < iqr_low || value > iqr_high)
                .count() as u64
        };

        let stdev_outlier_rounds = if stdev_ns <= f64::EPSILON {
            0
        } else {
            let low = mean_ns - STDEV_OUTLIER_FACTOR * stdev_ns;
            let high = mean_ns + STDEV_OUTLIER_FACTOR * stdev_ns;
            self.per_iter_ns
                .iter()
                .filter(|&&value| value < low || value > high)
                .count() as u64
        };

        (iqr_outlier_rounds, stdev_outlier_rounds)
    }
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }

    let clamped_q = q.clamp(0.0, 1.0);
    let pos = clamped_q * (sorted.len() as f64 - 1.0);
    let lower = pos.floor() as usize;
    let upper = pos.ceil() as usize;

    if lower == upper {
        sorted[lower]
    } else {
        let weight = pos - lower as f64;
        sorted[lower] * (1.0 - weight) + sorted[upper] * weight
    }
}

#[derive(Debug, Deserialize)]
struct SavedSample {
    #[serde(default)]
    _sampling_mode: Option<serde_json::Value>,
    iters: Vec<f64>,
    times: Vec<f64>,
}

#[derive(Debug, Deserialize)]
struct EstimatesRoot {
    mean: Option<Estimate>,
    median: Option<Estimate>,
    #[serde(rename = "std_dev")]
    std_dev: Option<Estimate>,
}

#[derive(Debug, Deserialize)]
struct Estimate {
    point_estimate: f64,
}

#[derive(Debug, Deserialize)]
struct BenchmarkIdRecord {
    group_id: String,
    function_id: Option<String>,
    value_str: Option<String>,
}

struct BenchmarkIdentity {
    name: String,
    uri: String,
}
