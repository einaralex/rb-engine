use anyhow::{bail, Result};
use flate2::{write::ZlibEncoder, Compression};
use std::io::Write;

const MAX_FILE: usize = 32 * 1024 * 1024;

#[derive(Debug, Default)]
pub struct CueCells {
    pub quick_cues: Option<Vec<u8>>,
    pub loops: Option<Vec<u8>>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Cue {
    slot: u32,
    kind: u8,
    start_ms: u32,
    end_ms: u32,
    label: Vec<u8>,
    rgb: Option<[u8; 3]>,
}

fn u32be(raw: &[u8]) -> Result<u32> {
    Ok(u32::from_be_bytes(raw.try_into()?))
}

fn qcompress(raw: &[u8]) -> Result<Vec<u8>> {
    let mut result = (raw.len() as u32).to_be_bytes().to_vec();
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(raw)?;
    result.extend(encoder.finish()?);
    Ok(result)
}

fn parse_pco2(payload: &[u8]) -> Result<Vec<Cue>> {
    let mut at = 0;
    let mut cues = Vec::new();
    while at < payload.len() {
        if payload.len() - at < 40 || &payload[at..at + 4] != b"PCP2" {
            bail!("invalid PCP2 cue entry");
        }
        let len = u32be(&payload[at + 8..at + 12])? as usize;
        if len < 40 || len > payload.len() - at {
            bail!("invalid PCP2 cue length");
        }
        let entry = &payload[at..at + len];
        let mut label = Vec::new();
        let mut rgb = None;
        if len > 43 {
            let comment_len = u32be(&entry[40..44])? as usize;
            let end = 44usize
                .checked_add(comment_len)
                .ok_or_else(|| anyhow::anyhow!("cue comment length overflow"))?;
            if comment_len & 1 != 0 || end > len {
                bail!("invalid PCP2 UTF-16BE comment");
            }
            let utf16 = entry[44..end]
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .take_while(|c| *c != 0)
                .collect::<Vec<_>>();
            label = String::from_utf16_lossy(&utf16).into_bytes();
            if end + 4 <= len {
                rgb = Some([entry[end + 1], entry[end + 2], entry[end + 3]]);
            }
        }
        cues.push(Cue {
            slot: u32be(&entry[12..16])?,
            kind: entry[16],
            start_ms: u32be(&entry[20..24])?,
            end_ms: u32be(&entry[24..28])?,
            label,
            rgb,
        });
        at += len;
    }
    Ok(cues)
}

fn parse_file(data: &[u8]) -> Result<Vec<Cue>> {
    if data.len() < 12 || data.len() > MAX_FILE || &data[..4] != b"PMAI" {
        bail!("invalid ANLZ PMAI header");
    }
    let header = u32be(&data[4..8])? as usize;
    let declared = u32be(&data[8..12])? as usize;
    if header < 12 || header > data.len() || declared != data.len() {
        bail!("invalid ANLZ file length");
    }
    let mut at = header;
    let mut cues = Vec::new();
    while at < data.len() {
        if data.len() - at < 12 {
            bail!("truncated ANLZ section");
        }
        let section_header = u32be(&data[at + 4..at + 8])? as usize;
        let section_len = u32be(&data[at + 8..at + 12])? as usize;
        if section_header < 12 || section_len < section_header || section_len > data.len() - at {
            bail!("invalid ANLZ section length");
        }
        if &data[at..at + 4] == b"PCO2" {
            cues.extend(parse_pco2(&data[at + section_header..at + section_len])?);
        }
        at += section_len;
    }
    Ok(cues)
}

/// Converts the ANLZ PCO2 hot cue subset established by the Python reference.
/// The analyzer supplies beat grids, so PQTZ is not used in this workflow.
pub fn convert_cues(files: &[Vec<u8>], sample_rate: i64) -> Result<CueCells> {
    if sample_rate <= 0 {
        bail!("ANLZ cue conversion needs a positive sample rate");
    }
    let mut slots: [Option<Cue>; 8] = std::array::from_fn(|_| None);
    let mut warnings = Vec::new();
    for data in files {
        for cue in parse_file(data)? {
            if cue.slot == 0 {
                warnings.push("Rekordbox memory cue has no verified Engine slot mapping".into());
                continue;
            }
            if cue.slot > 8 {
                warnings.push(format!(
                    "Rekordbox hot cue {} exceeds eight Engine slots",
                    cue.slot
                ));
                continue;
            }
            if !matches!(cue.kind, 1 | 2) || (cue.kind == 2 && cue.end_ms <= cue.start_ms) {
                warnings.push(format!("unsupported Rekordbox hot cue {}", cue.slot));
                continue;
            }
            if cue.label.len() > 255 {
                warnings.push(format!("Rekordbox hot cue {} label is too long", cue.slot));
                continue;
            }
            let slot = (cue.slot - 1) as usize;
            if let Some(existing) = &slots[slot] {
                if existing != &cue {
                    bail!("conflicting Rekordbox hot cue slot {}", cue.slot);
                }
            } else {
                slots[slot] = Some(cue);
            }
        }
    }
    let mut quick_cues = 8i64.to_be_bytes().to_vec();
    let mut loops = 8i64.to_le_bytes().to_vec();
    let mut cue_count = 0;
    let mut loop_count = 0;
    for item in slots {
        let (label, start, end, color, kind) = if let Some(cue) = item {
            (
                cue.label,
                cue.start_ms as f64 * sample_rate as f64 / 1000.0,
                cue.end_ms as f64 * sample_rate as f64 / 1000.0,
                cue.rgb
                    .map(|rgb| [255, rgb[0], rgb[1], rgb[2]])
                    .unwrap_or([0; 4]),
                cue.kind,
            )
        } else {
            (Vec::new(), -1.0, -1.0, [0; 4], 0)
        };
        if kind == 1 {
            cue_count += 1;
        }
        if kind == 2 {
            loop_count += 1;
        }
        quick_cues.push(if kind == 1 { label.len() as u8 } else { 0 });
        if kind == 1 {
            quick_cues.extend_from_slice(&label);
        }
        quick_cues.extend_from_slice(&(if kind == 1 { start } else { -1.0 }).to_be_bytes());
        quick_cues.extend_from_slice(&(if kind == 1 { color } else { [0; 4] }));
        loops.push(if kind == 2 { label.len() as u8 } else { 0 });
        if kind == 2 {
            loops.extend_from_slice(&label);
        }
        loops.extend_from_slice(&(if kind == 2 { start } else { -1.0 }).to_le_bytes());
        loops.extend_from_slice(&(if kind == 2 { end } else { -1.0 }).to_le_bytes());
        loops.extend_from_slice(&[if kind == 2 { 1 } else { 0 }, if kind == 2 { 1 } else { 0 }]);
        loops.extend_from_slice(&(if kind == 2 { color } else { [0; 4] }));
    }
    quick_cues.extend_from_slice(&(-1.0f64).to_be_bytes());
    quick_cues.push(0);
    quick_cues.extend_from_slice(&(-1.0f64).to_be_bytes());
    Ok(CueCells {
        quick_cues: if cue_count > 0 {
            Some(qcompress(&quick_cues)?)
        } else {
            None
        },
        loops: if loop_count > 0 { Some(loops) } else { None },
        warnings,
    })
}
