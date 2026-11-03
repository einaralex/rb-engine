use anyhow::Result;
use rekordbox_to_engine::{
    analyzer::Analyzer,
    capture::{decode_stream, Analysis},
    onelibrary::read_onelibrary,
    plan::{build_plan, TrackPlan},
    workflow::prepare,
};
use std::{
    fs,
    path::{Path, PathBuf},
};

struct CapturedAnalyzer;
impl Analyzer for CapturedAnalyzer {
    fn analyze(&self, track: &TrackPlan, output: &Path) -> Result<Analysis> {
        fs::create_dir(output)?;
        let fixture: &[u8] = match track.content_id {
            1 => include_bytes!("fixtures/plasma-received.bin"),
            2 => include_bytes!("fixtures/ritmo-received.bin"),
            3 => include_bytes!("fixtures/bjarki-received.bin"),
            _ => anyhow::bail!("unexpected content ID"),
        };
        decode_stream(fixture)
    }
}

#[test]
#[ignore = "requires a disposable copy of the three-track Rekordbox USB"]
fn imports_three_track_reference_with_captured_packets() {
    let target = PathBuf::from(std::env::var("REKORDBOX_TO_ENGINE_INTEGRATION_USB").unwrap());
    assert!(target
        .to_string_lossy()
        .starts_with("/private/tmp/rekordbox-to-engine-integration-"));
    let source = read_onelibrary(&target.join("PIONEER/rekordbox/exportLibrary.db")).unwrap();
    let plan = build_plan(&target, source).unwrap();
    assert_eq!(plan.tracks.len(), 3);
    let capture_root =
        PathBuf::from(std::env::var("REKORDBOX_TO_ENGINE_INTEGRATION_CAPTURES").unwrap());
    let report = prepare(&plan, &CapturedAnalyzer, &capture_root).unwrap();
    assert!(report.verification.ok);
    assert_eq!(report.export.tracks, 3);
    assert_eq!(report.export.playlists, 1);
    assert_eq!(report.export.rgb_sidecars, 3);
}
