use anyhow::{bail, Context, Result};
use clap::Parser;
use rb_engine::{
    analyzer::OfflineAnalyzer, onelibrary::read_onelibrary, plan::build_plan, workflow::prepare,
};
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "rb-engine",
    about = "Prepare a Rekordbox USB for Engine DJ players"
)]
struct Args {
    #[arg(long)]
    target: PathBuf,
    #[arg(long)]
    dry_run: bool,
    #[arg(
        long,
        default_value = "/Applications/Engine DJ.app/Contents/MacOS/OfflineAnalyzer"
    )]
    analyzer: PathBuf,
    #[arg(long)]
    capture_output_dir: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let root = args
        .target
        .canonicalize()
        .context("target USB is not accessible")?;
    let modern = ["PIONEER", ".PIONEER"]
        .into_iter()
        .map(|name| root.join(name).join("rekordbox/exportLibrary.db"))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    if modern.len() != 1 {
        bail!("expected exactly one modern Rekordbox exportLibrary.db");
    }
    let source = read_onelibrary(&modern[0])?;
    let plan = build_plan(&root, source)?;
    if args.dry_run {
        println!("{}", serde_json::to_string_pretty(&plan)?);
        return Ok(());
    }
    if !args.analyzer.is_file() {
        bail!("OfflineAnalyzer is missing at {}", args.analyzer.display());
    }
    let analyzer = OfflineAnalyzer {
        executable: args.analyzer,
        min_tempo: 88,
        key_detection: saved_key_detection().unwrap_or(false),
        timeout: Duration::from_secs(180),
        idle_timeout: Duration::from_secs(30),
        idle_grace: Duration::from_secs(120),
    };
    let captures = args.capture_output_dir.unwrap_or_else(|| {
        std::env::temp_dir().join(format!("rb-engine-captures-{}", Uuid::new_v4()))
    });
    let report = prepare(&plan, &analyzer, &captures)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

fn saved_key_detection() -> Option<bool> {
    let home = std::env::var_os("HOME")?;
    let state = PathBuf::from(home).join(
        "Library/Application Support/AIR Music Technology/EnginePrime/State/PlanckClientState.json",
    );
    let data: serde_json::Value = serde_json::from_slice(&std::fs::read(state).ok()?).ok()?;
    let value = data.pointer("/data/Client/children/Librarian/children/Analyzer/children/AnalysisKeyDetection/string")?.as_str()?;
    match value {
        "On" => Some(true),
        "Off" => Some(false),
        _ => None,
    }
}
