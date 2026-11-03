use crate::plan::SourceLibrary;
use anyhow::{bail, Context, Result};
use flate2::read::ZlibDecoder;
use rusqlite::{Connection, OpenFlags};
use std::fs;
use std::io::Read;
use std::path::Path;

const KEY_BLOB: &str =
    "PN_1dH8$oLJY)16j_RvM6qphWw`476>;C1cWmI#se(PG`j}~xAjlufj?`#0i{;=glh(SkW)y0>n?YEiD`l%t(";
const KEY_XOR: &[u8] = b"657f48f84c437cc1";
const BASE85: &str =
    "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz!#$%&()*+-;<=>?@^_`{|}~";

fn derive_key() -> Result<String> {
    let mut decoded = Vec::new();
    for chunk in KEY_BLOB.as_bytes().chunks(5) {
        let mut value = 0u64;
        for byte in chunk {
            let digit = BASE85
                .as_bytes()
                .iter()
                .position(|b| b == byte)
                .context("invalid OneLibrary key encoding")?;
            value = value * 85 + digit as u64;
        }
        let word = (value as u32).to_be_bytes();
        decoded.extend_from_slice(&word[..if chunk.len() == 5 { 4 } else { chunk.len() - 1 }]);
    }
    for (i, byte) in decoded.iter_mut().enumerate() {
        *byte ^= KEY_XOR[i % KEY_XOR.len()];
    }
    let mut plain = String::new();
    ZlibDecoder::new(decoded.as_slice()).read_to_string(&mut plain)?;
    if !plain.starts_with("r8gd") {
        bail!("OneLibrary key derivation failed");
    }
    Ok(plain)
}

pub fn read_onelibrary(path: &Path) -> Result<SourceLibrary> {
    if !path.is_file() {
        bail!("Rekordbox exportLibrary.db is missing");
    }
    for suffix in ["-wal", "-journal"] {
        let sidecar = path.with_file_name(format!(
            "{}{}",
            path.file_name().unwrap().to_string_lossy(),
            suffix
        ));
        match fs::metadata(&sidecar) {
            Ok(metadata) if metadata.len() > 0 => {
                bail!(
                    "Rekordbox database has pending {suffix} data; close Rekordbox and export cleanly"
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("cannot inspect {}", sidecar.display()))
            }
        }
    }
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .context("cannot open Rekordbox OneLibrary")?;
    db.execute_batch("PRAGMA cipher = sqlcipher; PRAGMA legacy = 4;")?;
    db.pragma_update(None, "key", derive_key()?)?;
    let integrity = crate::engine::sqlite_rows(&db, "PRAGMA integrity_check;")?;
    if integrity[0]["integrity_check"] != "ok" {
        bail!("OneLibrary integrity check failed");
    }
    let tracks = crate::engine::sqlite_rows(
        &db,
        r#"SELECT c.*, artist.name AS artistName, album.name AS albumName,
       genre.name AS genreName, label.name AS labelName
FROM content c
LEFT JOIN artist ON c.artist_id_artist = artist.artist_id
LEFT JOIN album ON c.album_id = album.album_id
LEFT JOIN genre ON c.genre_id = genre.genre_id
LEFT JOIN label ON c.label_id = label.label_id
ORDER BY c.content_id;"#,
    )?;
    let playlists = crate::engine::sqlite_rows(&db,
        "SELECT playlist_id, name, attribute, playlist_id_parent, sequenceNo FROM playlist ORDER BY playlist_id;")?;
    let playlist_entries = crate::engine::sqlite_rows(&db,
        "SELECT playlist_id, content_id, sequenceNo FROM playlist_content ORDER BY playlist_id, sequenceNo;")?;
    let keys = crate::engine::sqlite_rows(&db, "SELECT key_id, name FROM \"key\";")?;
    let images = crate::engine::sqlite_rows(&db, "SELECT image_id, path FROM image;")?;
    let source = serde_json::json!({
        "tracks": tracks, "playlists": playlists, "playlist_entries": playlist_entries,
        "keys": keys, "images": images,
    });
    Ok(serde_json::from_value(source)?)
}

#[cfg(test)]
mod tests {
    #[test]
    fn derives_known_key_prefix() {
        assert!(super::derive_key().unwrap().starts_with("r8gd"));
    }

    #[test]
    fn reads_encrypted_onelibrary_without_external_sqlcipher() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exportLibrary.db");
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA cipher = sqlcipher; PRAGMA legacy = 4;")
            .unwrap();
        db.pragma_update(None, "key", super::derive_key().unwrap())
            .unwrap();
        db.execute_batch(r#"CREATE TABLE content (content_id INTEGER, path TEXT, artist_id_artist INTEGER, album_id INTEGER, genre_id INTEGER, label_id INTEGER);
CREATE TABLE artist (artist_id INTEGER, name TEXT);
CREATE TABLE album (album_id INTEGER, name TEXT);
CREATE TABLE genre (genre_id INTEGER, name TEXT);
CREATE TABLE label (label_id INTEGER, name TEXT);
CREATE TABLE playlist (playlist_id INTEGER, name TEXT, attribute INTEGER, playlist_id_parent INTEGER, sequenceNo INTEGER);
CREATE TABLE playlist_content (playlist_id INTEGER, content_id INTEGER, sequenceNo INTEGER);
CREATE TABLE "key" (key_id INTEGER, name TEXT);
CREATE TABLE image (image_id INTEGER, path TEXT);
INSERT INTO content (content_id,path) VALUES (42,'/Contents/song.wav');
INSERT INTO playlist VALUES (7,'Set',0,NULL,1);
INSERT INTO playlist_content VALUES (7,42,1);"#).unwrap();
        drop(db);
        let source = super::read_onelibrary(&path).unwrap();
        assert_eq!(source.tracks[0].content_id, 42);
        assert_eq!(source.playlists[0].name, "Set");
        assert_eq!(source.playlist_entries[0].content_id, 42);
    }
}
