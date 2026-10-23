use crate::plan::SourceLibrary;
use anyhow::{bail, Context, Result};
use flate2::read::ZlibDecoder;
use serde_json::Value;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

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

fn file_uri(path: &Path) -> Result<String> {
    let absolute = path.canonicalize()?;
    let mut encoded = String::from("file://");
    for b in absolute.to_string_lossy().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(b) {
            encoded.push(*b as char);
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    encoded.push_str("?mode=ro&immutable=1");
    Ok(encoded)
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
    let key = derive_key()?.replace('\'', "''");
    let sql = format!(
        r#"PRAGMA cipher = sqlcipher;
PRAGMA legacy = 4;
PRAGMA key = '{key}';
PRAGMA integrity_check;
SELECT c.*, artist.name AS artistName, album.name AS albumName,
       genre.name AS genreName, label.name AS labelName
FROM content c
LEFT JOIN artist ON c.artist_id_artist = artist.artist_id
LEFT JOIN album ON c.album_id = album.album_id
LEFT JOIN genre ON c.genre_id = genre.genre_id
LEFT JOIN label ON c.label_id = label.label_id
ORDER BY c.content_id;
SELECT playlist_id, name, attribute, playlist_id_parent, sequenceNo FROM playlist ORDER BY playlist_id;
SELECT playlist_id, content_id, sequenceNo FROM playlist_content ORDER BY playlist_id, sequenceNo;
SELECT key_id, name FROM "key" UNION ALL SELECT NULL, NULL WHERE NOT EXISTS(SELECT 1 FROM "key");
SELECT image_id, path FROM image UNION ALL SELECT NULL, NULL WHERE NOT EXISTS(SELECT 1 FROM image);
"#
    );
    let mut child = Command::new("sqlcipher")
        .args(["-readonly", "-batch", "-bail", "-json", &file_uri(path)?])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("SQLCipher CLI is required to read Rekordbox OneLibrary")?;
    child.stdin.take().unwrap().write_all(sql.as_bytes())?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        bail!("SQLCipher could not read exportLibrary.db");
    }
    let arrays: Vec<Value> = serde_json::Deserializer::from_slice(&output.stdout)
        .into_iter::<Value>()
        .collect::<std::result::Result<_, _>>()?;
    if arrays.len() != 7 {
        bail!("unexpected OneLibrary query result count: {}", arrays.len());
    }
    if arrays[0]
        .as_array()
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("ok"))
        .and_then(Value::as_str)
        != Some("ok")
        || arrays[1]
            .as_array()
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("integrity_check"))
            .and_then(Value::as_str)
            != Some("ok")
    {
        bail!("OneLibrary integrity check failed");
    }
    let source = serde_json::json!({
        "tracks": arrays[2], "playlists": arrays[3], "playlist_entries": arrays[4],
        "keys": arrays[5].as_array().unwrap().iter().filter(|row| !row["key_id"].is_null()).collect::<Vec<_>>(),
        "images": arrays[6].as_array().unwrap().iter().filter(|row| !row["image_id"].is_null()).collect::<Vec<_>>(),
    });
    Ok(serde_json::from_value(source)?)
}

#[cfg(test)]
mod tests {
    #[test]
    fn derives_known_key_prefix() {
        assert!(super::derive_key().unwrap().starts_with("r8gd"));
    }
}
