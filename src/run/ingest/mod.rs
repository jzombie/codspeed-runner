use crate::prelude::*;
use clap::Args;
use std::path::PathBuf;

pub mod criterion;

#[derive(Args, Debug, Clone)]
pub struct IngestArgs {
    /// Path to the Criterion `target/criterion` directory (or parent of it)
    #[arg(long)]
    pub criterion_dir: PathBuf,

    /// Profile folder to write CodSpeed artifacts into. If omitted, a temporary folder will be used.
    #[arg(long)]
    pub profile_folder: Option<PathBuf>,

    /// After ingestion, upload the produced profile folder to CodSpeed using the current config
    #[arg(long, default_value_t = false)]
    pub upload: bool,
}
pub async fn ingest_criterion(args: IngestArgs) -> Result<PathBuf> {
    let profile_folder = if let Some(p) = args.profile_folder {
        p
    } else {
        // create a temporary directory
        let tmp = tempfile::tempdir()?;
        tmp.path().to_path_buf()
    };

    criterion::ingest_criterion_results(&args.criterion_dir, &profile_folder)
        .context("Failed to ingest Criterion results")?;

    info!(
        "Wrote CodSpeed profile artifacts to {}",
        profile_folder.display()
    );
    Ok(profile_folder)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn test_ingest_samples() {
        let samples_dir = PathBuf::from(format!(
            "{}/src/run/ingest/samples/criterion_target",
            env!("CARGO_MANIFEST_DIR")
        ));
        let tmp = tempdir().unwrap();
        let profile = tmp.path().to_path_buf();

        criterion::ingest_criterion_results(&samples_dir, &profile).unwrap();

        let bench_file = profile.join("codspeed-benchmarks.json");
        assert!(bench_file.exists());
        let content = std::fs::read_to_string(bench_file).unwrap();
        assert!(content.contains("bench1"));
        assert!(content.contains("bench2"));

        let metadata = profile.join("perf.metadata");
        assert!(metadata.exists());
    }
}
