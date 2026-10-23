use crate::analyzer::Analyzer;
use crate::engine::{verify_engine, write_engine_with_hashes, ExportResult, VerifyReport};
use crate::plan::Plan;
use anyhow::{bail, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize)]
pub struct TrackAnalysisSummary {
    pub content_id: i64,
    pub bpm: Option<f64>,
    pub key: i32,
    pub overview_entries: usize,
    pub rgb_entries: usize,
}

#[derive(Debug, Serialize)]
pub struct PrepareReport {
    pub capture_root: PathBuf,
    pub export: ExportResult,
    pub verification: VerifyReport,
    pub analysis: Vec<TrackAnalysisSummary>,
}

fn hash_file(path: &Path) -> Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut chunk = [0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        hash.update(&chunk[..n]);
    }
    Ok(hash.finalize().to_vec())
}

pub fn prepare(plan: &Plan, analyzer: &dyn Analyzer, capture_root: &Path) -> Result<PrepareReport> {
    if plan.tracks.is_empty() {
        bail!("Rekordbox USB has no tracks to analyze");
    }
    if plan.target.join("Engine Library").exists() {
        bail!("Engine Library already exists");
    }
    let parent = capture_root
        .parent()
        .context("capture output has no parent")?
        .canonicalize()?;
    if parent.starts_with(&plan.target) {
        bail!("capture output must be outside the USB target");
    }
    fs::create_dir(capture_root).context("capture output directory already exists")?;
    let mut analyses = Vec::with_capacity(plan.tracks.len());
    let mut audio_hashes = Vec::with_capacity(plan.tracks.len());
    let mut summaries = Vec::with_capacity(plan.tracks.len());
    for (index, track) in plan.tracks.iter().enumerate() {
        let before = hash_file(&track.audio)?;
        let capture = capture_root.join(format!("{:04}-{}", index + 1, track.content_id));
        eprintln!(
            "Analyzing {}/{}: {}",
            index + 1,
            plan.tracks.len(),
            track.title
        );
        let analysis = analyzer
            .analyze(track, &capture)
            .with_context(|| format!("analysis failed for track {}", track.content_id))?;
        let after = hash_file(&track.audio)?;
        if before != after {
            bail!("audio changed while analyzing track {}", track.content_id);
        }
        audio_hashes.push(hex::encode(after));
        summaries.push(TrackAnalysisSummary {
            content_id: track.content_id,
            bpm: analysis.bpm,
            key: analysis.key,
            overview_entries: analysis.overview_entries,
            rgb_entries: analysis.rgb_entries,
        });
        analyses.push(analysis);
    }
    let export = write_engine_with_hashes(plan, &analyses, &audio_hashes)?;
    let verification = verify_engine(&plan.target)?;
    if !verification.ok {
        bail!(
            "published Engine library failed verification: {:?}",
            verification.issues
        );
    }
    Ok(PrepareReport {
        capture_root: capture_root.to_path_buf(),
        export,
        verification,
        analysis: summaries,
    })
}
