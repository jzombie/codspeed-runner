use crate::prelude::*;
use runner_shared::metadata::PerfMetadata;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

/// Ingest Criterion results from `criterion_dir` and write CodSpeed-friendly artifacts into `profile_folder`.
pub fn ingest_criterion_results(criterion_dir: &Path, profile_folder: &Path) -> Result<()> {
    // Walk criterion_dir recursively for per-benchmark folders.
    if !criterion_dir.exists() {
        bail!(
            "Criterion directory does not exist: {}",
            criterion_dir.display()
        );
    }

    let mut collected_dirs: Vec<PathBuf> = vec![];

    fn collect_benchmark_dirs(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let estimates = path.join("new").join("estimates.json");
            let raw = path.join("new").join("raw.csv");
            if estimates.exists() || raw.exists() {
                out.push(path.clone());
                continue;
            }

            // Recurse into subdirectory
            collect_benchmark_dirs(&path, out)?;
        }
        Ok(())
    }

    collect_benchmark_dirs(criterion_dir, &mut collected_dirs)?;

    // Now parse each collected benchmark dir
    let mut benchmarks: Vec<(String, f64)> = Vec::new();
    for path in collected_dirs {
        let estimates = path.join("new").join("estimates.json");
        if estimates.exists() {
            if let Ok(time) = parse_estimates_json(&estimates) {
                let name = path.file_name().unwrap().to_string_lossy().to_string();
                benchmarks.push((name, time));
                continue;
            }
        }

        let raw = path.join("new").join("raw.csv");
        if raw.exists() {
            if let Ok(time) = parse_raw_csv_mean(&raw) {
                let name = path.file_name().unwrap().to_string_lossy().to_string();
                benchmarks.push((name, time));
                continue;
            }
        }
    }

    // Deduplicate by name (in case of nested duplicates)
    let mut unique: Vec<(String, f64)> = Vec::new();
    for (n, t) in benchmarks.drain(..) {
        if !unique.iter().any(|(un, _)| un == &n) {
            unique.push((n, t));
        }
    }

    benchmarks = unique;

    // Ensure profile folder exists
    fs::create_dir_all(profile_folder)?;

    // Write a simple codspeed-benchmarks.json with name/time pairs
    let bench_json_path = profile_folder.join("codspeed-benchmarks.json");
    let mut bench_json = serde_json::Map::new();
    let array = benchmarks
        .iter()
        .map(|(n, t)| {
            let mut m = serde_json::Map::new();
            m.insert("name".into(), serde_json::Value::String(n.clone()));
            m.insert(
                "time".into(),
                serde_json::Value::Number(
                    serde_json::Number::from_f64(*t).unwrap_or_else(|| serde_json::Number::from(0)),
                ),
            );
            serde_json::Value::Object(m)
        })
        .collect::<Vec<_>>();
    bench_json.insert("benchmarks".into(), serde_json::Value::Array(array));
    fs::write(bench_json_path, serde_json::to_string_pretty(&bench_json)?)?;

    // Write minimal perf.metadata so other tools can detect URIs
    let metadata = PerfMetadata {
        version: 1,
        integration: ("criterion-ingest".into(), env!("CARGO_PKG_VERSION").into()),
        uri_by_ts: benchmarks
            .iter()
            .map(|(n, _)| (current_time(), n.clone()))
            .collect(),
        ignored_modules: vec![],
        markers: vec![],
    };
    metadata.save_to(profile_folder)?;

    Ok(())
}

fn current_time() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Deserialize)]
struct EstimatesRoot {
    mean: Option<Estimate>,
    median: Option<Estimate>,
}

#[derive(Deserialize)]
struct Estimate {
    point_estimate: f64,
}

fn parse_estimates_json(path: &Path) -> Result<f64> {
    let data = fs::read_to_string(path)?;
    let root: EstimatesRoot =
        serde_json::from_str(&data).context("Failed to parse estimates.json")?;
    if let Some(mean) = root.mean {
        return Ok(mean.point_estimate);
    }
    if let Some(median) = root.median {
        return Ok(median.point_estimate);
    }
    bail!("No mean/median estimate found in {}", path.display());
}

fn parse_raw_csv_mean(path: &Path) -> Result<f64> {
    let data = fs::read_to_string(path)?;
    let mut sum = 0f64;
    let mut count = 0u64;
    for line in data.lines() {
        // skip header if present
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        // CSV may have multiple columns, try last column as value
        let cols: Vec<&str> = line.split(',').collect();
        if let Some(s) = cols.last() {
            if let Ok(v) = s.trim().parse::<f64>() {
                sum += v;
                count += 1;
            }
        }
    }
    if count == 0 {
        bail!("No numeric rows found in {}", path.display());
    }
    Ok(sum / (count as f64))
}
