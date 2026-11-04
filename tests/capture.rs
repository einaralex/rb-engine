use rekordbox_to_engine::capture::{decode_stream, request};

#[test]
fn request_matches_proven_worker_request() {
    let audio =
        "/Volumes/BANGERS/Contents/Arthur Robert/Frame Dragging/Arthur Robert - Plasma Waves.aiff";
    assert_eq!(
        request(audio, 88, true).unwrap(),
        include_bytes!("fixtures/plasma-request.bin")
    );
}

#[test]
fn decodes_finished_capture_without_audio_fixture() {
    let raw = include_bytes!("fixtures/plasma-received.bin");
    let analysis = decode_stream(raw).unwrap();
    assert_eq!(analysis.sample_rate, 44_100.0);
    assert_eq!(analysis.samples, 15_861_101);
    assert_eq!(analysis.bpm, Some(140.0));
    assert_eq!(analysis.key, 17);
    assert_eq!(analysis.overview_entries, 1024);
    assert_eq!(analysis.rgb_entries, 1024);
    assert!(!analysis.beat_data.is_empty());
    assert!(!analysis.track_data.is_empty());
}

#[test]
fn rejects_incomplete_capture() {
    let mut raw = include_bytes!("fixtures/plasma-received.bin").to_vec();
    raw.truncate(raw.len() - 32);
    assert!(decode_stream(&raw).is_err());
}
