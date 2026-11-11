use anyhow::{bail, Result};
use rekordbox_to_engine::{
    analyzer::Analyzer,
    capture::{decode_stream, Analysis},
    engine::{validate_existing, verify_engine},
    plan::{build_plan, build_plan_with_options, SourceLibrary, TrackPlan},
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

fn fixture_plan(target: &Path) -> rekordbox_to_engine::plan::Plan {
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

fn plan_with_tracks(target: &Path, ids: &[i64]) -> rekordbox_to_engine::plan::Plan {
    plan_with_tracks_options(target, ids, false)
}

fn plan_with_tracks_options(
    target: &Path,
    ids: &[i64],
    replace_existing: bool,
) -> rekordbox_to_engine::plan::Plan {
    let tracks = ids
        .iter()
        .map(|id| {
            let path = format!("/Contents/Test/{id}.wav");
            let media = target.join(path.trim_start_matches('/'));
            fs::create_dir_all(media.parent().unwrap()).unwrap();
            fs::write(media, format!("fixture audio {id}")).unwrap();
            json!({"content_id": id, "path": path, "title": format!("Track {id}")})
        })
        .collect::<Vec<_>>();
    let source: SourceLibrary = serde_json::from_value(json!({
        "tracks": tracks,
        "playlists": [{"playlist_id": 1, "name": "Set", "attribute": 0, "playlist_id_parent": 0}],
        "playlist_entries": ids.iter().map(|id| json!({"playlist_id": 1, "content_id": id})).collect::<Vec<_>>()
    })).unwrap();
    build_plan_with_options(target, source, replace_existing).unwrap()
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

#[test]
fn second_export_refreshes_managed_library_and_tracks_current_selection() {
    let usb = tempfile::tempdir().unwrap();
    let captures = tempfile::tempdir().unwrap();
    let first = plan_with_tracks(usb.path(), &[1, 2]);
    prepare(
        &first,
        &FixtureAnalyzer { fail: false },
        &captures.path().join("first"),
    )
    .unwrap();
    assert_eq!(verify_engine(usb.path()).unwrap().tracks, 2);

    let second = plan_with_tracks(usb.path(), &[2, 3]);
    let report = prepare(
        &second,
        &FixtureAnalyzer { fail: false },
        &captures.path().join("second"),
    )
    .unwrap();
    assert!(report.verification.ok);
    assert_eq!(report.verification.tracks, 2);
    assert!(!usb.path().join(".rekordbox-to-engine-previous").exists());
    let state: serde_json::Value = serde_json::from_slice(
        &fs::read(usb.path().join("Engine Library/.engine-usb.json")).unwrap(),
    )
    .unwrap();
    assert!(state["track_ids"].get("rekordbox:1").is_none());
    assert!(state["track_ids"].get("rekordbox:2").is_some());
    assert!(state["track_ids"].get("rekordbox:3").is_some());
}

#[test]
fn failed_refresh_keeps_previous_engine_library() {
    let usb = tempfile::tempdir().unwrap();
    let captures = tempfile::tempdir().unwrap();
    let first = plan_with_tracks(usb.path(), &[1]);
    prepare(
        &first,
        &FixtureAnalyzer { fail: false },
        &captures.path().join("first"),
    )
    .unwrap();
    let database = usb.path().join("Engine Library/Database2/m.db");
    let before = fs::read(&database).unwrap();

    let second = plan_with_tracks(usb.path(), &[1, 2]);
    assert!(prepare(
        &second,
        &FixtureAnalyzer { fail: true },
        &captures.path().join("second")
    )
    .is_err());
    assert_eq!(fs::read(database).unwrap(), before);
    assert!(verify_engine(usb.path()).unwrap().ok);
    assert!(!usb.path().join(".rekordbox-to-engine-previous").exists());
}

struct IncompleteAnalyzer;
impl Analyzer for IncompleteAnalyzer {
    fn analyze(&self, _track: &TrackPlan, output: &Path) -> Result<Analysis> {
        fs::create_dir(output)?;
        let mut analysis = decode_stream(include_bytes!("fixtures/plasma-received.bin"))?;
        analysis.rgb_sidecar.clear();
        Ok(analysis)
    }
}

#[test]
fn invalid_staged_refresh_keeps_previous_engine_library() {
    let usb = tempfile::tempdir().unwrap();
    let captures = tempfile::tempdir().unwrap();
    let first = plan_with_tracks(usb.path(), &[1]);
    prepare(
        &first,
        &FixtureAnalyzer { fail: false },
        &captures.path().join("first"),
    )
    .unwrap();
    let database = usb.path().join("Engine Library/Database2/m.db");
    let before = fs::read(&database).unwrap();

    let second = plan_with_tracks(usb.path(), &[1, 2]);
    assert!(prepare(
        &second,
        &IncompleteAnalyzer,
        &captures.path().join("second")
    )
    .is_err());
    assert_eq!(fs::read(database).unwrap(), before);
    assert!(verify_engine(usb.path()).unwrap().ok);
}

#[test]
fn interrupted_swap_restores_previous_library_before_planning() {
    let usb = tempfile::tempdir().unwrap();
    let captures = tempfile::tempdir().unwrap();
    let first = plan_with_tracks(usb.path(), &[1]);
    prepare(
        &first,
        &FixtureAnalyzer { fail: false },
        &captures.path().join("first"),
    )
    .unwrap();
    fs::rename(
        usb.path().join("Engine Library"),
        usb.path().join(".rekordbox-to-engine-previous"),
    )
    .unwrap();
    validate_existing(usb.path()).unwrap();
    assert!(verify_engine(usb.path()).unwrap().ok);
    assert!(!usb.path().join(".rekordbox-to-engine-previous").exists());
}

#[test]
fn refuses_unmanaged_or_modified_engine_library() {
    let usb = tempfile::tempdir().unwrap();
    fs::create_dir(usb.path().join("Engine Library")).unwrap();
    assert!(validate_existing(usb.path())
        .unwrap_err()
        .to_string()
        .contains("not managed"));
    fs::remove_dir_all(usb.path().join("Engine Library")).unwrap();
    let captures = tempfile::tempdir().unwrap();
    let first = plan_with_tracks(usb.path(), &[1]);
    prepare(
        &first,
        &FixtureAnalyzer { fail: false },
        &captures.path().join("first"),
    )
    .unwrap();
    let database = usb.path().join("Engine Library/Database2/m.db");
    let mut bytes = fs::read(&database).unwrap();
    bytes[100] ^= 1;
    fs::write(database, bytes).unwrap();
    assert!(validate_existing(usb.path())
        .unwrap_err()
        .to_string()
        .contains("was modified"));
}

#[test]
fn refuses_to_discard_engine_side_history() {
    let usb = tempfile::tempdir().unwrap();
    let captures = tempfile::tempdir().unwrap();
    let first = plan_with_tracks(usb.path(), &[1]);
    prepare(
        &first,
        &FixtureAnalyzer { fail: false },
        &captures.path().join("first"),
    )
    .unwrap();
    let history = usb.path().join("Engine Library/Database2/hm.db");
    rusqlite::Connection::open(history)
        .unwrap()
        .execute("INSERT INTO Historylist(id) VALUES (1)", [])
        .unwrap();
    assert!(validate_existing(usb.path())
        .unwrap_err()
        .to_string()
        .contains("Engine-side data"));
}

#[test]
fn explicit_replacement_accepts_unmanaged_library() {
    let usb = tempfile::tempdir().unwrap();
    let captures = tempfile::tempdir().unwrap();
    let old = usb.path().join("Engine Library");
    fs::create_dir(&old).unwrap();
    fs::write(old.join("player-notes"), b"old library").unwrap();
    assert!(rekordbox_to_engine::engine::replacement_reason(usb.path())
        .unwrap()
        .is_some());
    let plan = plan_with_tracks_options(usb.path(), &[1], true);
    prepare(
        &plan,
        &FixtureAnalyzer { fail: false },
        &captures.path().join("new"),
    )
    .unwrap();
    assert!(verify_engine(usb.path()).unwrap().ok);
    assert!(!old.join("player-notes").exists());
}

#[test]
fn explicit_replacement_failure_preserves_unmanaged_library() {
    let usb = tempfile::tempdir().unwrap();
    let captures = tempfile::tempdir().unwrap();
    let old = usb.path().join("Engine Library");
    fs::create_dir(&old).unwrap();
    fs::write(old.join("player-notes"), b"old library").unwrap();
    let plan = plan_with_tracks_options(usb.path(), &[1], true);
    assert!(prepare(
        &plan,
        &FixtureAnalyzer { fail: true },
        &captures.path().join("new")
    )
    .is_err());
    assert_eq!(fs::read(old.join("player-notes")).unwrap(), b"old library");
}
