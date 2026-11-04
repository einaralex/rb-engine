use rekordbox_to_engine::plan::{build_plan, SourceLibrary};
use serde_json::json;
use std::fs;

#[test]
fn plans_existing_usb_audio_and_playlist_without_writing() {
    let usb = tempfile::tempdir().unwrap();
    let media = usb.path().join("Contents/Bjarki/Blessuð Börnin.wav");
    fs::create_dir_all(media.parent().unwrap()).unwrap();
    fs::write(&media, b"short test audio").unwrap();
    let before = fs::read_dir(usb.path()).unwrap().count();
    let source: SourceLibrary = serde_json::from_value(json!({
        "tracks": [{"content_id": 7, "path": "/Contents/Bjarki/Blessuð Börnin.wav", "title": "Blessuð Börnin", "bpmx100": 15600}],
        "playlists": [{"playlist_id": 3, "name": "Test", "attribute": 0, "playlist_id_parent": 0, "sequenceNo": 1}],
        "playlist_entries": [{"playlist_id": 3, "content_id": 7, "sequenceNo": 1}]
    })).unwrap();
    let plan = build_plan(usb.path(), source).unwrap();
    assert_eq!(plan.tracks.len(), 1);
    assert_eq!(
        plan.tracks[0].engine_path,
        "../Contents/Bjarki/Blessuð Börnin.wav"
    );
    assert_eq!(plan.playlists[0].track_ids, vec![7]);
    assert_eq!(plan.min_tempo, 88);
    assert!(!usb.path().join("Engine Library").exists());
    assert_eq!(fs::read_dir(usb.path()).unwrap().count(), before);
}

#[test]
fn refuses_audio_paths_outside_contents() {
    let usb = tempfile::tempdir().unwrap();
    let source: SourceLibrary = serde_json::from_value(json!({
        "tracks": [{"content_id": 7, "path": "/../elsewhere.wav"}],
        "playlists": [], "playlist_entries": []
    }))
    .unwrap();
    assert!(build_plan(usb.path(), source).is_err());
}

#[test]
fn nested_playlists_keep_rekordbox_depth_first_order() {
    let usb = tempfile::tempdir().unwrap();
    let media = usb.path().join("Contents/song.wav");
    fs::create_dir_all(media.parent().unwrap()).unwrap();
    fs::write(media, b"a").unwrap();
    let source: SourceLibrary = serde_json::from_value(json!({
        "tracks": [{"content_id": 1, "path": "/Contents/song.wav"}],
        "playlists": [
            {"playlist_id": 1, "name": "Folder", "attribute": 1, "playlist_id_parent": 0, "sequenceNo": 1},
            {"playlist_id": 2, "name": "Sibling", "attribute": 0, "playlist_id_parent": 0, "sequenceNo": 2},
            {"playlist_id": 3, "name": "Child", "attribute": 0, "playlist_id_parent": 1, "sequenceNo": 1}
        ],
        "playlist_entries": [{"playlist_id": 3, "content_id": 1, "sequenceNo": 1}]
    })).unwrap();
    let plan = build_plan(usb.path(), source).unwrap();
    assert_eq!(
        plan.playlists
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Folder", "Child", "Sibling"]
    );
}
