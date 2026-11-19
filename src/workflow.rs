use crate::analyzer::Analyzer;
use crate::engine::{verify_engine, write_engine_with_hashes, ExportResult, VerifyReport};
use crate::plan::Plan;
use anyhow::{bail, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;

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
    prepare_with_jobs(plan, analyzer, capture_root, 4)
}

pub fn prepare_with_jobs(
    plan: &Plan,
    analyzer: &dyn Analyzer,
    capture_root: &Path,
    jobs: usize,
) -> Result<PrepareReport> {
    if plan.tracks.is_empty() {
        bail!("Rekordbox USB has no tracks to analyze");
    }
    if jobs == 0 {
        bail!("parallel job count must be at least one");
    }
    crate::engine::validate_for_export(&plan.target, plan.replace_existing)?;
    let parent = capture_root
        .parent()
        .context("capture output has no parent")?
        .canonicalize()?;
    if parent.starts_with(&plan.target) {
        bail!("capture output must be outside the USB target");
    }
    fs::create_dir(capture_root).context("capture output directory already exists")?;
    let worker_count = jobs.min(plan.tracks.len());
    let next_index = AtomicUsize::new(0);
    let cancel = AtomicBool::new(false);
    let (sender, receiver) = mpsc::channel();
    thread::scope(|scope| {
        for _ in 0..worker_count {
            let sender = sender.clone();
            let next_index = &next_index;
            let cancel = &cancel;
            scope.spawn(move || loop {
                if cancel.load(Ordering::Acquire) {
                    break;
                }
                let index = next_index.fetch_add(1, Ordering::Relaxed);
                if index >= plan.tracks.len() {
                    break;
                }
                let track = &plan.tracks[index];
                eprintln!(
                    "Analyzing {}/{}: {}",
                    index + 1,
                    plan.tracks.len(),
                    track.title
                );
                let outcome = (|| -> Result<(crate::capture::Analysis, String)> {
                    let before = hash_file(&track.audio)?;
                    let capture =
                        capture_root.join(format!("{:04}-{}", index + 1, track.content_id));
                    let analysis = analyzer.analyze(track, &capture).with_context(|| {
                        format!("analysis failed for track {}", track.content_id)
                    })?;
                    let after = hash_file(&track.audio)?;
                    if before != after {
                        bail!("audio changed while analyzing track {}", track.content_id);
                    }
                    Ok((analysis, hex::encode(after)))
                })();
                if outcome.is_err() {
                    cancel.store(true, Ordering::Release);
                }
                if sender.send((index, outcome)).is_err() {
                    break;
                }
            });
        }
        drop(sender);
    });

    let mut results: Vec<Option<Result<(crate::capture::Analysis, String)>>> =
        (0..plan.tracks.len()).map(|_| None).collect();
    for (index, result) in receiver {
        results[index] = Some(result);
    }
    let mut analyses = Vec::with_capacity(plan.tracks.len());
    let mut audio_hashes = Vec::with_capacity(plan.tracks.len());
    let mut summaries = Vec::with_capacity(plan.tracks.len());
    for (index, result) in results.into_iter().enumerate() {
        let (analysis, audio_hash) =
            result.context("analysis cancelled before every track completed")??;
        let track = &plan.tracks[index];
        summaries.push(TrackAnalysisSummary {
            content_id: track.content_id,
            bpm: analysis.bpm,
            key: analysis.key,
            overview_entries: analysis.overview_entries,
            rgb_entries: analysis.rgb_entries,
        });
        analyses.push(analysis);
        audio_hashes.push(audio_hash);
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
