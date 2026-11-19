//! Real OfflineAnalyzer check using generated audio and a disposable USB tree.

#[cfg(target_os = "macos")]
mod macos {
    use anyhow::Result;
    use rekordbox_to_engine::{
        analyzer::{Analyzer, OfflineAnalyzer},
        capture::Analysis,
        plan::{build_plan, SourceLibrary, TrackPlan},
        workflow::prepare_with_jobs,
    };
    use serde_json::json;
    use std::{
        fs,
        path::Path,
        sync::atomic::{AtomicUsize, Ordering},
        time::{Duration, Instant},
    };

    const RATE: u32 = 44_100;

    fn write_click_wav(path: &Path, seconds: u32) {
        let frames = RATE * seconds;
        let data_len = frames * 4;
        let mut bytes = Vec::with_capacity(44 + data_len as usize);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&2_u16.to_le_bytes());
        bytes.extend_from_slice(&RATE.to_le_bytes());
        bytes.extend_from_slice(&(RATE * 4).to_le_bytes());
        bytes.extend_from_slice(&4_u16.to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for frame in 0..frames {
            let click = frame % (RATE / 2);
            let sample = if click < RATE / 50 {
                (12_000 * ((RATE / 50 - click) as i32) / (RATE as i32 / 50)) as i16
            } else {
                0
            };
            bytes.extend_from_slice(&sample.to_le_bytes());
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        fs::write(path, bytes).unwrap();
    }

    struct CountedAnalyzer {
        inner: OfflineAnalyzer,
        active: AtomicUsize,
        max_active: AtomicUsize,
    }

    impl Analyzer for CountedAnalyzer {
        fn analyze(&self, track: &TrackPlan, output: &Path) -> Result<Analysis> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            let result = self.inner.analyze(track, output);
            self.active.fetch_sub(1, Ordering::SeqCst);
            result
        }
    }

    fn captured_sample_counts(root: &Path) -> Vec<i64> {
        (1..=4)
            .map(|id| {
                let report = root.join(format!("{id:04}-{id}/report.json"));
                let value: serde_json::Value =
                    serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
                value["samples"].as_i64().unwrap()
            })
            .collect()
    }

    #[test]
    #[ignore = "requires Engine DJ Desktop's OfflineAnalyzer on macOS"]
    fn four_live_analyzers_prepare_and_refresh_disposable_usb() {
        let executable = Path::new("/Applications/Engine DJ.app/Contents/MacOS/OfflineAnalyzer");
        assert!(executable.is_file(), "install Engine DJ Desktop first");
        let usb = tempfile::tempdir().unwrap();
        let captures = tempfile::tempdir().unwrap();
        let media_dir = usb.path().join("Contents/Test");
        fs::create_dir_all(&media_dir).unwrap();
        for id in 1..=4 {
            write_click_wav(&media_dir.join(format!("{id}.wav")), 22 + 2 * id);
        }
        let source: SourceLibrary = serde_json::from_value(json!({
            "tracks": (1..=4).map(|id| json!({
                "content_id": id,
                "path": format!("/Contents/Test/{id}.wav"),
                "title": format!("Click {id}")
            })).collect::<Vec<_>>(),
            "playlists": [], "playlist_entries": []
        }))
        .unwrap();
        let plan = build_plan(usb.path(), source).unwrap();
        let analyzer = CountedAnalyzer {
            inner: OfflineAnalyzer {
                executable: executable.to_path_buf(),
                min_tempo: 88,
                key_detection: false,
                timeout: Duration::from_secs(180),
                idle_timeout: Duration::from_secs(30),
                idle_grace: Duration::from_secs(120),
            },
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
        };
        let parallel_start = Instant::now();
        let parallel = prepare_with_jobs(&plan, &analyzer, &captures.path().join("parallel"), 4)
            .unwrap_or_else(|error| panic!("parallel preparation failed: {error:#}"));
        let parallel_time = parallel_start.elapsed();
        assert_eq!(analyzer.max_active.load(Ordering::SeqCst), 4);
        assert_eq!(parallel.verification.tracks, 4);
        assert!(parallel.verification.ok);
        assert_eq!(
            parallel
                .analysis
                .iter()
                .map(|track| track.content_id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        let parallel_samples = captured_sample_counts(&captures.path().join("parallel"));
        assert!(
            parallel_samples
                .windows(2)
                .all(|counts| counts[0] < counts[1]),
            "capture lengths were mismatched: {parallel_samples:?}"
        );

        let serial_start = Instant::now();
        let serial = prepare_with_jobs(&plan, &analyzer, &captures.path().join("serial"), 1)
            .unwrap_or_else(|error| panic!("serial refresh failed: {error:#}"));
        let serial_time = serial_start.elapsed();
        assert!(serial.verification.ok);
        assert_eq!(serial.verification.tracks, 4);
        assert_eq!(
            parallel_samples,
            captured_sample_counts(&captures.path().join("serial"))
        );
        eprintln!(
            "4 tracks: parallel fresh export {parallel_time:?}, serial refresh {serial_time:?}"
        );
    }
}
