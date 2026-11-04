use crate::anlz::{convert_cues, CueCells};
use crate::capture::Analysis;
use crate::plan::{Plan, TrackPlan};
use anyhow::{bail, Context, Result};
use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use rusqlite::{types::ValueRef, Connection, OpenFlags};
use serde::Serialize;
use sha1::{Digest as Sha1Digest, Sha1};
use sha2::Sha256;
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const MAIN_SCHEMA: &str = include_str!("../sql/m.sql");
const HISTORY_SCHEMA: &str = include_str!("../sql/hm.sql");
const SECONDARY_SCHEMA: &str = include_str!("../sql/secondary.sql");

#[derive(Debug, Serialize)]
pub struct ExportResult {
    pub engine_library: PathBuf,
    pub database_uuid: String,
    pub tracks: usize,
    pub playlists: usize,
    pub rgb_sidecars: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct VerifyReport {
    pub ok: bool,
    pub tracks: usize,
    pub playlists: usize,
    pub rgb_sidecars: usize,
    pub issues: Vec<String>,
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}
fn opt_str(s: Option<&str>) -> String {
    s.map(quote).unwrap_or_else(|| "NULL".to_owned())
}
fn opt_i64(n: Option<i64>) -> String {
    n.map(|v| v.to_string())
        .unwrap_or_else(|| "NULL".to_owned())
}
fn blob(data: &[u8]) -> String {
    format!("X'{}'", hex::encode(data))
}

fn insert(table: &str, cells: &[(&str, String)]) -> String {
    format!(
        "INSERT INTO \"{table}\" ({}) VALUES ({});\n",
        cells
            .iter()
            .map(|(name, _)| format!("\"{name}\""))
            .collect::<Vec<_>>()
            .join(","),
        cells
            .iter()
            .map(|(_, value)| value.as_str())
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn sqlite_script(path: &Path, sql: &str) -> Result<()> {
    let db = Connection::open(path).with_context(|| format!("cannot create {}", path.display()))?;
    db.execute_batch(sql)
        .with_context(|| format!("cannot write {}", path.display()))?;
    Ok(())
}

fn sqlite_query(path: &Path, query: &str) -> Result<serde_json::Value> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("cannot open {}", path.display()))?;
    sqlite_rows(&db, query)
}

pub(crate) fn sqlite_rows(db: &Connection, query: &str) -> Result<serde_json::Value> {
    let mut statement = db.prepare(query)?;
    let names: Vec<String> = statement
        .column_names()
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let mut rows = statement.query([])?;
    let mut result = Vec::new();
    while let Some(row) = rows.next()? {
        let mut object = serde_json::Map::new();
        for (index, name) in names.iter().enumerate() {
            let value = match row.get_ref(index)? {
                ValueRef::Null => serde_json::Value::Null,
                ValueRef::Integer(n) => serde_json::Value::from(n),
                ValueRef::Real(n) => serde_json::Value::from(n),
                ValueRef::Text(bytes) => serde_json::Value::from(std::str::from_utf8(bytes)?),
                ValueRef::Blob(bytes) => {
                    serde_json::Value::from(String::from_utf8_lossy(bytes).into_owned())
                }
            };
            object.insert(name.clone(), value);
        }
        result.push(serde_json::Value::Object(object));
    }
    Ok(serde_json::Value::Array(result))
}

fn qcompress(raw: &[u8]) -> Result<Vec<u8>> {
    if raw.len() > u32::MAX as usize {
        bail!("Qt cell too large");
    }
    let mut out = (raw.len() as u32).to_be_bytes().to_vec();
    let mut writer = ZlibEncoder::new(Vec::new(), Compression::default());
    writer.write_all(raw)?;
    out.extend(writer.finish()?);
    Ok(out)
}

fn patch_track_key(cell: &[u8]) -> Result<Vec<u8>> {
    if cell.len() < 5 {
        bail!("trackData cell is truncated");
    }
    let expected = u32::from_be_bytes(cell[..4].try_into()?) as usize;
    let mut raw = Vec::new();
    ZlibDecoder::new(&cell[4..]).read_to_end(&mut raw)?;
    if raw.len() != expected || raw.len() < 20 {
        bail!("trackData cell is malformed");
    }
    raw[16..20].copy_from_slice(&(-1i32).to_be_bytes());
    qcompress(&raw)
}

fn empty_quick_cues() -> Result<Vec<u8>> {
    let mut raw = 8i64.to_be_bytes().to_vec();
    for _ in 0..8 {
        raw.push(0);
        raw.extend_from_slice(&(-1.0f64).to_be_bytes());
        raw.extend_from_slice(&[0; 4]);
    }
    raw.extend_from_slice(&(-1.0f64).to_be_bytes());
    raw.push(0);
    raw.extend_from_slice(&(-1.0f64).to_be_bytes());
    qcompress(&raw)
}

fn empty_loops() -> Vec<u8> {
    let mut raw = 8i64.to_le_bytes().to_vec();
    for _ in 0..8 {
        raw.push(0);
        raw.extend_from_slice(&(-1.0f64).to_le_bytes());
        raw.extend_from_slice(&(-1.0f64).to_le_bytes());
        raw.extend_from_slice(&[0; 6]);
    }
    raw
}

fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn artwork(plan: &Plan) -> Result<(String, HashMap<PathBuf, i64>, i64)> {
    let mut rows = String::new();
    let mut ids = HashMap::new();
    for track in &plan.tracks {
        if let Some(path) = &track.image_path {
            if ids.contains_key(path) {
                continue;
            }
            let resolved = path
                .canonicalize()
                .context("Rekordbox artwork is missing")?;
            if !resolved.starts_with(&plan.target) || !resolved.is_file() {
                bail!("artwork path is outside the USB");
            }
            let data = fs::read(&resolved)?;
            let digest = hex::encode(Sha1::digest(&data));
            let id = ids.len() as i64 + 1;
            rows.push_str(&insert(
                "AlbumArt",
                &[
                    ("id", id.to_string()),
                    ("hash", quote(&digest)),
                    ("albumArt", blob(&data)),
                ],
            ));
            ids.insert(path.clone(), id);
        }
    }
    let placeholder = ids.len() as i64 + 1;
    if plan.tracks.iter().any(|t| t.image_path.is_none()) {
        rows.push_str(&insert(
            "AlbumArt",
            &[
                ("id", placeholder.to_string()),
                ("hash", "NULL".into()),
                ("albumArt", "NULL".into()),
            ],
        ));
    }
    Ok((rows, ids, placeholder))
}

fn imported_key(name: Option<&str>, analyzed: i32) -> i32 {
    match name {
        Some("4A") => 17,
        Some("8A") => 1,
        _ => {
            if analyzed >= 0 {
                analyzed
            } else {
                -1
            }
        }
    }
}

fn engine_track_ids(plan: &Plan) -> HashMap<i64, i64> {
    plan.tracks
        .iter()
        .enumerate()
        .map(|(index, track)| (track.content_id, index as i64 + 1))
        .collect()
}

fn engine_playlist_ids(plan: &Plan) -> HashMap<i64, i64> {
    plan.playlists
        .iter()
        .enumerate()
        .map(|(index, playlist)| (playlist.playlist_id, index as i64 + 1))
        .collect()
}

fn rekordbox_cues(track: &TrackPlan) -> Result<CueCells> {
    let Some(base) = &track.anlz_path else {
        return Ok(CueCells::default());
    };
    let Some(sample_rate) = track.sample_rate else {
        return Ok(CueCells {
            warnings: vec![format!(
                "track {} has ANLZ but no sample rate",
                track.content_id
            )],
            ..Default::default()
        });
    };
    let mut files = Vec::new();
    let mut candidates = vec![base.clone()];
    for suffix in ["DAT", "EXT", "dat", "ext"] {
        candidates.push(base.with_extension(suffix));
    }
    candidates.sort();
    candidates.dedup();
    for path in candidates {
        if !path.exists() {
            continue;
        }
        if path.is_symlink() {
            bail!("ANLZ sidecar is a symlink: {}", path.display());
        }
        let resolved = path.canonicalize()?;
        if !resolved.starts_with(
            track
                .audio
                .ancestors()
                .find(|p| p.join("PIONEER").exists() || p.join(".PIONEER").exists())
                .context("USB root not found")?,
        ) {
            bail!("ANLZ sidecar is outside USB");
        }
        if fs::metadata(&path)?.len() > 32 * 1024 * 1024 {
            bail!("ANLZ sidecar is too large");
        }
        files.push(fs::read(path)?);
    }
    if files.is_empty() {
        return Ok(CueCells {
            warnings: vec![format!(
                "track {} ANLZ sidecar is missing",
                track.content_id
            )],
            ..Default::default()
        });
    }
    convert_cues(&files, sample_rate)
}

fn track_sql(
    track: &TrackPlan,
    analysis: &Analysis,
    engine_id: i64,
    uuid: &str,
    art_id: i64,
    counter: Option<i64>,
) -> Result<(String, Vec<String>)> {
    let id = engine_id;
    let key = imported_key(track.key_name.as_deref(), analysis.key);
    let mut fields: Vec<(&str, String)> = vec![
        ("id", id.to_string()),
        ("playOrder", "NULL".into()),
        (
            "length",
            ((analysis.samples as f64 / analysis.sample_rate).ceil() as i64).to_string(),
        ),
        ("bpm", (track.bpm_rekordbox + 0.5).floor().to_string()),
        ("year", opt_i64(track.year)),
        ("path", quote(&track.engine_path)),
        (
            "filename",
            quote(
                Path::new(&track.engine_path)
                    .file_name()
                    .unwrap()
                    .to_str()
                    .context("invalid filename")?,
            ),
        ),
        ("bitrate", "NULL".into()),
        (
            "bpmAnalyzed",
            analysis.bpm.unwrap_or(track.bpm_rekordbox).to_string(),
        ),
        ("albumArtId", art_id.to_string()),
        ("fileBytes", track.file_bytes.to_string()),
        ("title", quote(&track.title)),
        ("artist", opt_str(track.artist.as_deref())),
        ("album", opt_str(track.album.as_deref())),
        ("genre", opt_str(track.genre.as_deref())),
        ("comment", opt_str(track.comment.as_deref())),
        ("label", opt_str(track.label.as_deref())),
        ("key", key.to_string()),
        ("rating", (track.rating.unwrap_or(0) * 20).to_string()),
        ("isPlayed", "0".into()),
        ("fileType", "NULL".into()),
        ("isAnalyzed", "1".into()),
        ("isAvailable", "1".into()),
        ("isMetadataOfPackedTrackChanged", "0".into()),
        ("isPerfomanceDataOfPackedTrackChanged", "0".into()),
        ("isMetadataImported", "0".into()),
        ("pdbImportKey", opt_i64(counter)),
        ("streamingFlags", "0".into()),
        ("isBeatGridLocked", "0".into()),
        ("originDatabaseUuid", quote(uuid)),
        ("originTrackId", id.to_string()),
        ("explicitLyrics", "0".into()),
    ];
    let mut sql = insert("Track", &fields);
    fields.clear();
    let track_data = if matches!(track.key_name.as_deref(), Some("4A" | "8A")) {
        patch_track_key(&analysis.track_data)?
    } else {
        analysis.track_data.clone()
    };
    let cues = rekordbox_cues(track)?;
    sql.push_str(&insert(
        "PerformanceData",
        &[
            ("trackId", id.to_string()),
            ("trackData", blob(&track_data)),
            ("overviewWaveFormData", blob(&analysis.overview_data)),
            ("beatData", blob(&analysis.beat_data)),
            (
                "quickCues",
                blob(&cues.quick_cues.unwrap_or(empty_quick_cues()?)),
            ),
            ("loops", blob(&cues.loops.unwrap_or_else(empty_loops))),
            ("thirdPartySourceId", "NULL".into()),
            ("activeOnLoadLoops", "0".into()),
        ],
    ));
    Ok((sql, cues.warnings))
}

fn import_counter(target: &Path) -> Option<i64> {
    for name in ["PIONEER", ".PIONEER"] {
        let Ok(header) = fs::read(target.join(name).join("rekordbox/export.pdb")) else {
            continue;
        };
        if header.len() >= 28 && &header[..8] == [0, 0, 0, 0, 0, 16, 0, 0].as_slice() {
            return Some(u32::from_le_bytes(header[20..24].try_into().ok()?) as i64);
        }
    }
    None
}

fn main_db_sql(plan: &Plan, analyses: &[Analysis], uuid: &str) -> Result<(String, Vec<String>)> {
    let (base_schema, triggers) = MAIN_SCHEMA
        .split_once("CREATE TRIGGER ")
        .context("main schema is missing triggers")?;
    let mut sql = String::from("PRAGMA foreign_keys=ON;\nBEGIN;\n");
    sql.push_str(base_schema);
    sql.push_str("PRAGMA user_version=4194305;\n");
    sql.push_str(&insert(
        "Information",
        &[
            ("id", "1".into()),
            ("uuid", quote(uuid)),
            ("schemaVersionMajor", "3".into()),
            ("schemaVersionMinor", "0".into()),
            ("schemaVersionPatch", "2".into()),
            ("currentPlayedIndiciator", "0".into()),
            (
                "lastRekordBoxLibraryImportReadCounter",
                opt_i64(import_counter(&plan.target)),
            ),
        ],
    ));
    let (art_sql, art_ids, placeholder) = artwork(plan)?;
    sql.push_str(&art_sql);
    let mut warnings = Vec::new();
    let track_ids = engine_track_ids(plan);
    let playlist_ids = engine_playlist_ids(plan);
    for (track, analysis) in plan.tracks.iter().zip(analyses) {
        let art_id = track
            .image_path
            .as_ref()
            .and_then(|path| art_ids.get(path))
            .copied()
            .unwrap_or(placeholder);
        let (row_sql, row_warnings) = track_sql(
            track,
            analysis,
            track_ids[&track.content_id],
            uuid,
            art_id,
            import_counter(&plan.target),
        )?;
        sql.push_str(&row_sql);
        warnings.extend(row_warnings);
    }
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let mut siblings: BTreeMap<Option<i64>, Vec<i64>> = BTreeMap::new();
    for p in &plan.playlists {
        siblings.entry(p.parent_id).or_default().push(p.playlist_id);
    }
    let mut nexts = HashMap::new();
    for group in siblings.values() {
        for (index, id) in group.iter().enumerate() {
            nexts.insert(*id, group.get(index + 1).copied().unwrap_or(0));
        }
    }
    let mut entity_id = 1i64;
    for p in &plan.playlists {
        let engine_playlist_id = playlist_ids[&p.playlist_id];
        sql.push_str(&insert(
            "Playlist",
            &[
                ("id", engine_playlist_id.to_string()),
                ("title", quote(&p.name)),
                (
                    "parentListId",
                    p.parent_id
                        .map(|id| playlist_ids[&id])
                        .unwrap_or(0)
                        .to_string(),
                ),
                ("isPersisted", "1".into()),
                (
                    "nextListId",
                    if nexts[&p.playlist_id] == 0 {
                        0
                    } else {
                        playlist_ids[&nexts[&p.playlist_id]]
                    }
                    .to_string(),
                ),
                ("lastEditTime", timestamp.to_string()),
                ("isExplicitlyExported", "1".into()),
            ],
        ));
        for (index, track_id) in p.track_ids.iter().enumerate() {
            sql.push_str(&insert(
                "PlaylistEntity",
                &[
                    ("id", entity_id.to_string()),
                    ("listId", engine_playlist_id.to_string()),
                    ("trackId", track_ids[track_id].to_string()),
                    ("databaseUuid", quote(uuid)),
                    (
                        "nextEntityId",
                        if index + 1 < p.track_ids.len() {
                            entity_id + 1
                        } else {
                            0
                        }
                        .to_string(),
                    ),
                    ("membershipReference", "0".into()),
                ],
            ));
            entity_id += 1;
        }
    }
    sql.push_str("CREATE TRIGGER ");
    sql.push_str(triggers);
    sql.push_str("\nCOMMIT;\n");
    Ok((sql, warnings))
}

fn companion_db(path: &Path, schema: &str) -> Result<()> {
    let uuid = Uuid::new_v4().to_string();
    let sql = format!(
        "BEGIN;\n{schema}\nPRAGMA user_version=4194305;\n{}COMMIT;\n",
        insert(
            "Information",
            &[
                ("id", "1".into()),
                ("uuid", quote(&uuid)),
                ("schemaVersionMajor", "3".into()),
                ("schemaVersionMinor", "0".into()),
                ("schemaVersionPatch", "2".into())
            ]
        )
    );
    sqlite_script(path, &sql)
}

pub fn write_engine(plan: &Plan, analyses: &[Analysis]) -> Result<ExportResult> {
    let audio_hashes = plan
        .tracks
        .iter()
        .map(|track| hash_file(&track.audio))
        .collect::<Result<Vec<_>>>()?;
    write_engine_with_hashes(plan, analyses, &audio_hashes)
}

pub(crate) fn write_engine_with_hashes(
    plan: &Plan,
    analyses: &[Analysis],
    audio_hashes: &[String],
) -> Result<ExportResult> {
    if analyses.len() != plan.tracks.len() || analyses.is_empty() {
        bail!("one finished analysis is required per track");
    }
    if audio_hashes.len() != plan.tracks.len() {
        bail!("one audio hash is required per track");
    }
    let root = plan.target.join("Engine Library");
    if root.exists() {
        bail!("Engine Library already exists");
    }
    let stage = plan
        .target
        .join(format!(".rekordbox-to-engine-stage-{}", Uuid::new_v4()));
    fs::create_dir(&stage)?;
    let result = (|| -> Result<ExportResult> {
        let uuid = Uuid::new_v4().to_string();
        let database = stage.join("Database2");
        fs::create_dir_all(&database)?;
        let (main_sql, warnings) = main_db_sql(plan, analyses, &uuid)?;
        sqlite_script(&database.join("m.db"), &main_sql)?;
        companion_db(&database.join("hm.db"), HISTORY_SCHEMA)?;
        companion_db(&database.join("sm.db"), SECONDARY_SCHEMA)?;
        companion_db(&database.join("stm.db"), SECONDARY_SCHEMA)?;
        let rgb_dir = database.join("OverviewData").join(&uuid);
        fs::create_dir_all(&rgb_dir)?;
        let mut audio_state = Vec::new();
        let mut rgb_state = Vec::new();
        let track_ids = engine_track_ids(plan);
        let playlist_ids = engine_playlist_ids(plan);
        for ((track, analysis), audio_hash) in plan.tracks.iter().zip(analyses).zip(audio_hashes) {
            if fs::metadata(&track.audio)?.len() != track.file_bytes {
                bail!("audio changed during analysis: {}", track.audio.display());
            }
            let engine_id = track_ids[&track.content_id];
            let relative = format!("Database2/OverviewData/{uuid}/{engine_id}.rgb");
            let sidecar = stage.join(&relative);
            fs::write(&sidecar, &analysis.rgb_sidecar)?;
            rgb_state.push(serde_json::json!({"track_id": engine_id, "path": relative,
                "bytes": analysis.rgb_sidecar.len(), "sha256": hash_file(&sidecar)?}));
            audio_state.push(
                serde_json::json!({"track_id": format!("rekordbox:{}", track.content_id),
                "path": track.engine_path, "bytes": track.file_bytes, "sha256": audio_hash}),
            );
        }
        let db_hash = hash_file(&database.join("m.db"))?;
        let state = serde_json::json!({"format":"engine-usb-v1", "database_uuid": uuid,
            "database_sha256": db_hash, "audio": audio_state, "rgb_overviews": rgb_state,
            "track_ids": plan.tracks.iter().map(|t| (format!("rekordbox:{}", t.content_id), track_ids[&t.content_id])).collect::<BTreeMap<_,_>>(),
            "playlist_ids": plan.playlists.iter().map(|p| (format!("rekordbox-playlist:{}", p.playlist_id), playlist_ids[&p.playlist_id])).collect::<BTreeMap<_,_>>(),
            "managed_files": []});
        fs::write(
            stage.join(".engine-usb.json"),
            serde_json::to_vec_pretty(&state)?,
        )?;
        let staged_report = verify_engine_root(&stage, &plan.target)?;
        if !staged_report.ok {
            bail!(
                "staged Engine library failed verification: {:?}",
                staged_report.issues
            );
        }
        fs::rename(&stage, &root)?;
        Ok(ExportResult {
            engine_library: root,
            database_uuid: uuid,
            tracks: plan.tracks.len(),
            playlists: plan.playlists.len(),
            rgb_sidecars: analyses.len(),
            warnings,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&stage);
    }
    result
}

fn verify_engine_root(root: &Path, target: &Path) -> Result<VerifyReport> {
    let db = root.join("Database2/m.db");
    let integrity = sqlite_query(&db, "PRAGMA integrity_check;")?;
    let mut issues = Vec::new();
    if integrity[0]["integrity_check"] != "ok" {
        issues.push("database integrity check failed".into());
    }
    let fk = sqlite_query(&db, "PRAGMA foreign_key_check;")?;
    if !fk.as_array().is_some_and(|v| v.is_empty()) {
        issues.push("foreign key check failed".into());
    }
    let uuid = sqlite_query(&db, "SELECT uuid FROM Information LIMIT 1;")?[0]["uuid"]
        .as_str()
        .context("database UUID missing")?
        .to_owned();
    let detailed = sqlite_query(&db, "SELECT Track.id AS id,path,fileBytes,isAnalyzed,isAvailable,length(trackData) AS track_data,length(beatData) AS beat_data,length(overviewWaveFormData) AS overview FROM Track LEFT JOIN PerformanceData ON PerformanceData.trackId=Track.id;")?;
    let tracks = detailed.as_array().context("invalid track query")?;
    if tracks.is_empty() {
        issues.push("no Engine tracks".into());
    }
    let mut rgb_count = 0;
    for track in tracks {
        let id = track["id"].as_i64().context("track ID missing")?;
        let media_path = track["path"].as_str().context("track path missing")?;
        let media = root.join(media_path);
        let resolved = media.canonicalize();
        if !resolved
            .as_ref()
            .is_ok_and(|p| p.starts_with(target) && p.is_file())
        {
            issues.push(format!("track {id} audio missing"));
        } else if fs::metadata(&media)?.len() != track["fileBytes"].as_u64().unwrap_or(0) {
            issues.push(format!("track {id} audio size mismatch"));
        }
        if track["isAnalyzed"] != 1
            || track["isAvailable"] != 1
            || track["track_data"].as_u64().unwrap_or(0) == 0
            || track["beat_data"].as_u64().unwrap_or(0) == 0
            || track["overview"].as_u64().unwrap_or(0) == 0
        {
            issues.push(format!("track {id} analysis incomplete"));
        }
        let rgb = root.join(format!("Database2/OverviewData/{uuid}/{id}.rgb"));
        if rgb.is_file() && fs::metadata(rgb)?.len() > 0 {
            rgb_count += 1;
        } else {
            issues.push(format!("track {id} RGB waveform missing"));
        }
    }
    let playlists = sqlite_query(&db, "SELECT COUNT(*) AS count FROM Playlist;")?[0]["count"]
        .as_u64()
        .unwrap_or(0) as usize;
    let unresolved = sqlite_query(&db, "SELECT COUNT(*) AS count FROM PlaylistEntity AS p LEFT JOIN Track AS t ON (p.databaseUuid,p.trackId)=(t.originDatabaseUuid,t.originTrackId) WHERE t.id IS NULL;")?[0]["count"].as_u64().unwrap_or(0);
    if unresolved != 0 {
        issues.push("playlist contains unresolved tracks".into());
    }
    Ok(VerifyReport {
        ok: issues.is_empty(),
        tracks: tracks.len(),
        playlists,
        rgb_sidecars: rgb_count,
        issues,
    })
}

pub fn verify_engine(target: &Path) -> Result<VerifyReport> {
    let target = target.canonicalize()?;
    verify_engine_root(&target.join("Engine Library"), &target)
}

#[cfg(test)]
mod tests {
    use super::import_counter;
    use std::fs;

    #[test]
    fn reads_import_counter_from_hidden_pioneer_root() {
        let usb = tempfile::tempdir().unwrap();
        let pioneer = usb.path().join(".PIONEER/rekordbox/export.pdb");
        fs::create_dir_all(pioneer.parent().unwrap()).unwrap();
        let mut header = [0u8; 28];
        header[..8].copy_from_slice(&[0, 0, 0, 0, 0, 16, 0, 0]);
        header[20..24].copy_from_slice(&42u32.to_le_bytes());
        fs::write(pioneer, header).unwrap();
        assert_eq!(import_counter(usb.path()), Some(42));
    }
}
