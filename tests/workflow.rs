use anyhow::{bail, Result};
use rb_engine::{
    analyzer::Analyzer,
    capture::{decode_stream, Analysis},
    plan::{build_plan, SourceLibrary, TrackPlan},
    workflow::prepare,
};
use serde_json::json;
use std::{fs, path::Path};

struct FixtureAnalyzer {
    fail: bool,
}
impl Analyzer for FixtureAnalyzer {
    fn analyze(&self, _track: &TrackPlan, output: &Path) -> Result<Analysis> {
        if self.fail {
            bail!("fixture analyzer failure");
        }
        fs::create_dir(output)?;
        decode_stream(include_bytes!("fixtures/plasma-received.bin"))
    }
}

fn fixture_plan(target: &Path) -> rb_engine::plan::Plan {
    let media = target.join("Contents/Test/short.wav");
    fs::create_dir_all(media.parent().unwrap()).unwrap();
    fs::write(media, b"tiny media stub").unwrap();
    let source: SourceLibrary = serde_json::from_value(json!({
        "tracks": [{"content_id": 1, "path": "/Contents/Test/short.wav", "title": "Test"}],
        "playlists": [], "playlist_entries": []
    }))
    .unwrap();
    build_plan(target, source).unwrap()
}

#[test]
fn full_workflow_uses_analyzer_interface_and_verifies() {
    let usb = tempfile::tempdir().unwrap();
    let captures = tempfile::tempdir().unwrap();
    let plan = fixture_plan(usb.path());
    let report = prepare(
        &plan,
        &FixtureAnalyzer { fail: false },
        &captures.path().join("new"),
    )
    .unwrap();
    assert!(report.verification.ok);
    assert_eq!(report.export.tracks, 1);
}

#[test]
fn analysis_failure_does_not_publish_engine_library() {
    let usb = tempfile::tempdir().unwrap();
    let captures = tempfile::tempdir().unwrap();
    let plan = fixture_plan(usb.path());
    assert!(prepare(
        &plan,
        &FixtureAnalyzer { fail: true },
        &captures.path().join("new")
    )
    .is_err());
    assert!(!usb.path().join("Engine Library").exists());
}
