use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
pub struct SourceLibrary {
    pub tracks: Vec<SourceTrack>,
    pub playlists: Vec<SourcePlaylist>,
    pub playlist_entries: Vec<SourceEntry>,
    #[serde(default)]
    pub keys: Vec<SourceKey>,
    #[serde(default)]
    pub images: Vec<SourceImage>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceTrack {
    pub content_id: i64,
    pub path: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(rename = "artistName", default)]
    pub artist_name: Option<String>,
    #[serde(rename = "albumName", default)]
    pub album_name: Option<String>,
    #[serde(rename = "genreName", default)]
    pub genre_name: Option<String>,
    #[serde(rename = "labelName", default)]
    pub label_name: Option<String>,
    #[serde(default)]
    pub bpmx100: Option<i64>,
    #[serde(rename = "djComment", default)]
    pub dj_comment: Option<String>,
    #[serde(rename = "releaseYear", default)]
    pub release_year: Option<i64>,
    #[serde(default)]
    pub rating: Option<i64>,
    #[serde(rename = "key_id", default)]
    pub key_id: Option<i64>,
    #[serde(rename = "image_id", default)]
    pub image_id: Option<i64>,
    #[serde(rename = "samplingRate", default)]
    pub sampling_rate: Option<i64>,
    #[serde(default)]
    pub length: Option<i64>,
    #[serde(rename = "analysisDataFilePath", default)]
    pub analysis_data_file_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourcePlaylist {
    pub playlist_id: i64,
    pub name: String,
    pub attribute: i64,
    #[serde(default)]
    pub playlist_id_parent: Option<i64>,
    #[serde(rename = "sequenceNo", default)]
    pub sequence_no: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceEntry {
    pub playlist_id: i64,
    pub content_id: i64,
    #[serde(rename = "sequenceNo", default)]
    pub sequence_no: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceKey {
    pub key_id: i64,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceImage {
    pub image_id: i64,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub target: PathBuf,
    pub min_tempo: u32,
    pub tracks: Vec<TrackPlan>,
    pub playlists: Vec<PlaylistPlan>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrackPlan {
    pub content_id: i64,
    pub title: String,
    pub audio: PathBuf,
    pub engine_path: String,
    pub file_bytes: u64,
    pub bpm_rekordbox: f64,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub label: Option<String>,
    pub comment: Option<String>,
    pub year: Option<i64>,
    pub rating: Option<i64>,
    pub key_name: Option<String>,
    pub image_path: Option<PathBuf>,
    pub anlz_path: Option<PathBuf>,
    pub sample_rate: Option<i64>,
    pub length: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlaylistPlan {
    pub playlist_id: i64,
    pub name: String,
    pub parent_id: Option<i64>,
    pub track_ids: Vec<i64>,
}

pub fn usb_relative(raw: &str) -> Result<PathBuf> {
    let raw = raw.strip_prefix("file://localhost/").unwrap_or(raw);
    if raw.starts_with("file://") {
        bail!("unsupported file URL");
    }
    let normalized = raw.replace('\\', "/");
    let trimmed = normalized.trim_start_matches('/');
    let path = PathBuf::from(trimmed);
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        bail!("unsafe USB path: {raw}");
    }
    Ok(path)
}

pub fn build_plan(target: &Path, source: SourceLibrary) -> Result<Plan> {
    let target = target.canonicalize().context("target USB does not exist")?;
    if !target.is_dir() {
        bail!("target must be a directory");
    }
    if target.join("Engine Library").exists() {
        bail!("target already contains Engine Library");
    }
    let keys: BTreeMap<_, _> = source
        .keys
        .into_iter()
        .map(|k| (k.key_id, k.name))
        .collect();
    let images: BTreeMap<_, _> = source
        .images
        .into_iter()
        .map(|i| (i.image_id, i.path))
        .collect();
    let mut tracks = Vec::new();
    let mut seen_ids = HashSet::new();
    let mut seen_audio = HashSet::new();
    for row in source.tracks {
        if row.content_id <= 0 || !seen_ids.insert(row.content_id) {
            bail!("invalid or duplicate content ID");
        }
        let relative = usb_relative(&row.path)?;
        if relative.components().next() != Some(Component::Normal("Contents".as_ref())) {
            bail!("track {} is outside Contents", row.content_id);
        }
        let candidate = target.join(&relative);
        if candidate.is_symlink() {
            bail!("track {} audio is a symlink", row.content_id);
        }
        let audio = candidate
            .canonicalize()
            .with_context(|| format!("audio missing for track {}", row.content_id))?;
        if !audio.starts_with(&target) || !audio.is_file() || !seen_audio.insert(audio.clone()) {
            bail!("track {} audio is invalid or duplicated", row.content_id);
        }
        let file_bytes = fs::metadata(&audio)?.len();
        let engine_path = format!("../{}", relative.to_string_lossy().replace('\\', "/"));
        let title = row.title.filter(|s| !s.is_empty()).unwrap_or_else(|| {
            candidate
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        });
        let image_path = row
            .image_id
            .and_then(|id| images.get(&id))
            .filter(|s| !s.is_empty())
            .map(|s| usb_relative(s))
            .transpose()?
            .map(|s| target.join(s));
        let anlz_path = row
            .analysis_data_file_path
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(usb_relative)
            .transpose()?
            .map(|s| target.join(s));
        tracks.push(TrackPlan {
            content_id: row.content_id,
            title,
            audio,
            engine_path,
            file_bytes,
            bpm_rekordbox: row.bpmx100.unwrap_or(0) as f64 / 100.0,
            artist: row.artist_name,
            album: row.album_name,
            genre: row.genre_name,
            label: row.label_name,
            comment: row.dj_comment,
            year: row.release_year,
            rating: row.rating,
            key_name: row.key_id.and_then(|id| keys.get(&id).cloned()),
            image_path,
            anlz_path,
            sample_rate: row.sampling_rate,
            length: row.length,
        });
    }
    if tracks.is_empty() {
        bail!("Rekordbox library contains no tracks");
    }
    let rows = source.playlists;
    let playlist_ids: HashSet<_> = rows.iter().map(|r| r.playlist_id).collect();
    if playlist_ids.len() != rows.len() {
        bail!("duplicate playlist IDs");
    }
    let mut entries = source.playlist_entries;
    entries.sort_by_key(|e| {
        (
            e.playlist_id,
            e.sequence_no.unwrap_or(i64::MAX),
            e.content_id,
        )
    });
    let mut by_playlist: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for entry in entries {
        if !playlist_ids.contains(&entry.playlist_id) || !seen_ids.contains(&entry.content_id) {
            bail!("invalid playlist membership");
        }
        by_playlist
            .entry(entry.playlist_id)
            .or_default()
            .push(entry.content_id);
    }
    let mut children: BTreeMap<Option<i64>, Vec<SourcePlaylist>> = BTreeMap::new();
    for row in rows {
        children
            .entry(row.playlist_id_parent.filter(|p| *p != 0))
            .or_default()
            .push(row);
    }
    for siblings in children.values_mut() {
        siblings.sort_by_key(|r| (r.sequence_no.unwrap_or(i64::MAX), r.playlist_id));
    }
    fn walk(
        parent: Option<i64>,
        children: &mut BTreeMap<Option<i64>, Vec<SourcePlaylist>>,
        entries: &mut BTreeMap<i64, Vec<i64>>,
        visited: &mut HashSet<i64>,
        out: &mut Vec<PlaylistPlan>,
    ) -> Result<()> {
        for row in children.remove(&parent).unwrap_or_default() {
            if row.playlist_id <= 0
                || row.name.is_empty()
                || !matches!(row.attribute, 0 | 1)
                || !visited.insert(row.playlist_id)
            {
                bail!("invalid playlist hierarchy");
            }
            let id = row.playlist_id;
            out.push(PlaylistPlan {
                playlist_id: id,
                name: row.name,
                parent_id: parent,
                track_ids: if row.attribute == 1 {
                    vec![]
                } else {
                    entries.remove(&id).unwrap_or_default()
                },
            });
            walk(Some(id), children, entries, visited, out)?;
        }
        Ok(())
    }
    let mut playlists = Vec::new();
    let mut visited = HashSet::new();
    walk(
        None,
        &mut children,
        &mut by_playlist,
        &mut visited,
        &mut playlists,
    )?;
    if visited.len() != playlist_ids.len() {
        bail!("playlist tree contains a cycle or missing parent");
    }
    Ok(Plan {
        target,
        min_tempo: 88,
        tracks,
        playlists,
    })
}
