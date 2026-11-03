use rekordbox_to_engine::{
    capture::decode_stream,
    engine::{verify_engine, write_engine},
    plan::{build_plan, SourceLibrary},
};
use serde_json::json;
use std::fs;
use std::io::Read;

fn anlz_section(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = tag.to_vec();
    out.extend_from_slice(&12u32.to_be_bytes());
    out.extend_from_slice(&(12u32 + payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

#[test]
fn writes_verified_engine_library_and_preserves_pioneer() {
    let usb = tempfile::tempdir().unwrap();
    let media = usb.path().join("Contents/Bjarki/Blessuð Börnin.wav");
    fs::create_dir_all(media.parent().unwrap()).unwrap();
    fs::write(&media, b"small synthetic fixture").unwrap();
    let pioneer = usb.path().join("PIONEER/rekordbox/export.pdb");
    fs::create_dir_all(pioneer.parent().unwrap()).unwrap();
    fs::write(&pioneer, b"pioneer bytes must survive").unwrap();
    let source: SourceLibrary = serde_json::from_value(json!({
        "tracks": [{"content_id": 1, "path": "/Contents/Bjarki/Blessuð Börnin.wav", "title": "Blessuð Börnin", "artistName": "Bjarki", "bpmx100": 7800}],
        "playlists": [{"playlist_id": 1, "name": "Set", "attribute": 0, "playlist_id_parent": 0, "sequenceNo": 1}],
        "playlist_entries": [{"playlist_id": 1, "content_id": 1, "sequenceNo": 1}]
    })).unwrap();
    let plan = build_plan(usb.path(), source).unwrap();
    let analysis = decode_stream(include_bytes!("fixtures/plasma-received.bin")).unwrap();
    let result = write_engine(&plan, &[analysis]).unwrap();
    assert_eq!(result.tracks, 1);
    assert_eq!(fs::read(&pioneer).unwrap(), b"pioneer bytes must survive");
    let report = verify_engine(usb.path()).unwrap();
    assert!(report.ok, "{report:?}");
    assert_eq!(report.tracks, 1);
    assert_eq!(report.playlists, 1);
    assert_eq!(report.rgb_sidecars, 1);
}

#[test]
fn engine_ids_are_sequential_even_when_rekordbox_ids_have_gaps() {
    let usb = tempfile::tempdir().unwrap();
    let media = usb.path().join("Contents/song.wav");
    fs::create_dir_all(media.parent().unwrap()).unwrap();
    fs::write(media, b"tiny").unwrap();
    let source: SourceLibrary = serde_json::from_value(json!({
        "tracks": [{"content_id": 42, "path": "/Contents/song.wav"}],
        "playlists": [{"playlist_id": 9, "name": "Set", "attribute": 0, "playlist_id_parent": 0}],
        "playlist_entries": [{"playlist_id": 9, "content_id": 42}]
    }))
    .unwrap();
    let plan = build_plan(usb.path(), source).unwrap();
    let analysis = decode_stream(include_bytes!("fixtures/plasma-received.bin")).unwrap();
    let result = write_engine(&plan, &[analysis]).unwrap();
    let db = result.engine_library.join("Database2/m.db");
    let output = std::process::Command::new("sqlite3").arg(db)
        .arg("SELECT Track.id,Playlist.id,PlaylistEntity.trackId FROM Track,Playlist,PlaylistEntity;")
        .output().unwrap();
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "1|1|1");
    assert!(result
        .engine_library
        .join(format!(
            "Database2/OverviewData/{}/1.rgb",
            result.database_uuid
        ))
        .is_file());
}

#[test]
fn rekordbox_hot_cue_reaches_engine_performance_data() {
    let usb = tempfile::tempdir().unwrap();
    let media = usb.path().join("Contents/song.wav");
    fs::create_dir_all(media.parent().unwrap()).unwrap();
    fs::write(media, b"tiny").unwrap();
    let mut entry = vec![0u8; 40];
    entry[..4].copy_from_slice(b"PCP2");
    entry[8..12].copy_from_slice(&40u32.to_be_bytes());
    entry[12..16].copy_from_slice(&1u32.to_be_bytes());
    entry[16] = 1;
    entry[20..24].copy_from_slice(&2000u32.to_be_bytes());
    let section = anlz_section(b"PCO2", &entry);
    let mut anlz = b"PMAI".to_vec();
    anlz.extend_from_slice(&12u32.to_be_bytes());
    anlz.extend_from_slice(&(12u32 + section.len() as u32).to_be_bytes());
    anlz.extend_from_slice(&section);
    let sidecar = usb.path().join("PIONEER/USBANLZ/ANLZ0000.DAT");
    fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
    fs::write(sidecar, anlz).unwrap();
    let source: SourceLibrary = serde_json::from_value(json!({
        "tracks": [{"content_id": 1, "path": "/Contents/song.wav", "samplingRate": 44100,
            "analysisDataFilePath": "/PIONEER/USBANLZ/ANLZ0000.DAT"}],
        "playlists": [], "playlist_entries": []
    }))
    .unwrap();
    let plan = build_plan(usb.path(), source).unwrap();
    let analysis = decode_stream(include_bytes!("fixtures/plasma-received.bin")).unwrap();
    let result = write_engine(&plan, &[analysis]).unwrap();
    let output = std::process::Command::new("sqlite3")
        .arg(result.engine_library.join("Database2/m.db"))
        .arg("SELECT hex(quickCues) FROM PerformanceData WHERE trackId=1;")
        .output()
        .unwrap();
    assert!(output.status.success());
    let cell = hex::decode(String::from_utf8(output.stdout).unwrap().trim()).unwrap();
    let mut raw = Vec::new();
    flate2::read::ZlibDecoder::new(&cell[4..])
        .read_to_end(&mut raw)
        .unwrap();
    assert_eq!(f64::from_be_bytes(raw[9..17].try_into().unwrap()), 88_200.0);
}
