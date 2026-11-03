pub mod analyzer;
pub mod anlz;
pub mod capture;
pub mod engine;
pub mod onelibrary;
pub mod plan;
pub mod workflow;

/// Prepare a verified Rekordbox USB export using the installed Engine DJ analyzer.
pub fn prepare_usb(target: &std::path::Path) -> anyhow::Result<workflow::PrepareReport> {
    use anyhow::{bail, Context};
    use std::time::Duration;

    let root = target
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
    let source = onelibrary::read_onelibrary(&modern[0])?;
    let plan = plan::build_plan(&root, source)?;
    let executable =
        std::path::PathBuf::from("/Applications/Engine DJ.app/Contents/MacOS/OfflineAnalyzer");
    if !executable.is_file() {
        bail!("OfflineAnalyzer is missing at {}", executable.display());
    }
    let analyzer = analyzer::OfflineAnalyzer {
        executable,
        min_tempo: 88,
        key_detection: saved_key_detection().unwrap_or(false),
        timeout: Duration::from_secs(180),
        idle_timeout: Duration::from_secs(30),
        idle_grace: Duration::from_secs(120),
    };
    let captures = std::env::temp_dir().join(format!(
        "rekordbox-to-engine-captures-{}",
        uuid::Uuid::new_v4()
    ));
    workflow::prepare(&plan, &analyzer, &captures)
}

fn saved_key_detection() -> Option<bool> {
    let home = std::env::var_os("HOME")?;
    let state = std::path::PathBuf::from(home).join(
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
