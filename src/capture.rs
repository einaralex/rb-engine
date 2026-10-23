use anyhow::{bail, Context, Result};
use flate2::{write::ZlibEncoder, Compression};
use std::io::Write;

const MAX_PACKET: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Analysis {
    pub sample_rate: f64,
    pub samples: i64,
    pub bpm: Option<f64>,
    pub key: i32,
    pub track_data: Vec<u8>,
    pub beat_data: Vec<u8>,
    pub overview_data: Vec<u8>,
    pub rgb_sidecar: Vec<u8>,
    pub overview_entries: usize,
    pub rgb_entries: usize,
}

fn u32le(s: &[u8]) -> Result<u32> {
    Ok(u32::from_le_bytes(s.try_into()?))
}
fn u64le(s: &[u8]) -> Result<u64> {
    Ok(u64::from_le_bytes(s.try_into()?))
}
fn i64be(s: &[u8]) -> Result<i64> {
    Ok(i64::from_be_bytes(s.try_into()?))
}
fn f64be(s: &[u8]) -> Result<f64> {
    Ok(f64::from_be_bytes(s.try_into()?))
}
fn f64le(s: &[u8]) -> Result<f64> {
    Ok(f64::from_le_bytes(s.try_into()?))
}

pub fn encode_frame(command: u32, payload: &[u8]) -> Result<Vec<u8>> {
    if payload.len() > MAX_PACKET {
        bail!("analyzer request too large");
    }
    let mut result = Vec::with_capacity(24 + payload.len());
    result.extend_from_slice(&0u32.to_le_bytes());
    result.extend_from_slice(&command.to_le_bytes());
    result.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    result.extend_from_slice(&crc32fast::hash(payload).to_le_bytes());
    result.extend_from_slice(&0u32.to_le_bytes());
    result.extend_from_slice(payload);
    Ok(result)
}

pub fn request(audio: &str, min_tempo: u32, key_detection: bool) -> Result<Vec<u8>> {
    if !(30..=250).contains(&min_tempo) {
        bail!("minimum tempo must be 30–250 BPM");
    }
    let mut payload = vec![0u8; 16];
    payload[..4].copy_from_slice(&min_tempo.to_le_bytes());
    payload[8] = key_detection as u8;
    payload[11] = 1; // RGB analysis
    payload.extend_from_slice(audio.as_bytes());
    encode_frame(1, &payload)
}

fn qcompress(raw: &[u8]) -> Result<Vec<u8>> {
    if raw.len() > u32::MAX as usize {
        bail!("Qt payload too large");
    }
    let mut out = (raw.len() as u32).to_be_bytes().to_vec();
    let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
    z.write_all(raw)?;
    out.extend(z.finish()?);
    Ok(out)
}

#[derive(Default)]
struct Wave {
    target: Option<i64>,
    scale: Option<f64>,
    entries: Vec<u8>,
    trailer: [u8; 3],
}

impl Wave {
    fn add(&mut self, chunk: &[u8], rgb: bool) -> Result<()> {
        if chunk.is_empty() {
            return Ok(());
        }
        if chunk.len() < 27 {
            bail!("waveform chunk too short");
        }
        let count = i64be(&chunk[..8])?;
        let target = i64be(&chunk[8..16])?;
        let scale = f64be(&chunk[16..24])?;
        if count < 0
            || target <= 0
            || !scale.is_finite()
            || scale <= 0.0
            || chunk.len() != 27 + count as usize * 3
        {
            bail!("invalid waveform chunk");
        }
        if self.target.is_none() {
            self.target = Some(target);
            self.scale = Some(scale);
        }
        if self.target != Some(target) || self.scale != Some(scale) {
            bail!("waveform chunks disagree");
        }
        if count > 0 {
            let room = target as usize - (self.entries.len() / 3).min(target as usize);
            let copied = (count as usize).min(room);
            self.entries.extend_from_slice(&chunk[24..24 + copied * 3]);
            if copied > 0 {
                let tail = &chunk[chunk.len() - 3..];
                for (dst, src) in self.trailer.iter_mut().zip(tail) {
                    if rgb {
                        *dst = *src;
                    } else {
                        *dst = (*dst).max(*src);
                    }
                }
            }
        }
        Ok(())
    }

    fn finish(&self) -> Result<(usize, Vec<u8>)> {
        let target = self.target.context("waveform packets missing")?;
        let count = self.entries.len() / 3;
        if count != target as usize {
            bail!("waveform is incomplete: {count}/{target}");
        }
        let mut raw = Vec::with_capacity(27 + self.entries.len());
        raw.extend_from_slice(&(count as i64).to_be_bytes());
        raw.extend_from_slice(&target.to_be_bytes());
        raw.extend_from_slice(&self.scale.unwrap().to_be_bytes());
        raw.extend_from_slice(&self.entries);
        raw.extend_from_slice(&self.trailer);
        Ok((count, qcompress(&raw)?))
    }
}

fn read_grid(raw: &[u8], cursor: &mut usize) -> Result<Vec<(f64, i64)>> {
    if raw.len() < *cursor + 8 {
        bail!("beat grid header truncated");
    }
    let count = i64be(&raw[*cursor..*cursor + 8])?;
    *cursor += 8;
    if count < 0 || count as usize > (raw.len() - *cursor) / 24 {
        bail!("invalid beat marker count");
    }
    let mut grid = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let offset = f64le(&raw[*cursor..*cursor + 8])?;
        let number = i64::from_le_bytes(raw[*cursor + 8..*cursor + 16].try_into()?);
        if !offset.is_finite() {
            bail!("invalid beat offset");
        }
        grid.push((offset, number));
        *cursor += 24;
    }
    Ok(grid)
}

fn beat_bpm(raw: &[u8]) -> Result<Option<f64>> {
    if raw.len() < 33 {
        bail!("beat packet too short");
    }
    let sample_rate = f64be(&raw[..8])?;
    if !sample_rate.is_finite() || sample_rate <= 0.0 || raw[16] != 1 {
        bail!("beat grid is unset");
    }
    let mut cursor = 17;
    let _default = read_grid(raw, &mut cursor)?;
    let adjusted = read_grid(raw, &mut cursor)?;
    let mut bpms = Vec::new();
    for pair in adjusted.windows(2) {
        let ds = pair[1].0 - pair[0].0;
        let db = pair[1].1 - pair[0].1;
        if ds > 0.0 && db > 0 {
            bpms.push(sample_rate * 60.0 * db as f64 / ds);
        }
    }
    if bpms.is_empty() {
        return Ok(None);
    }
    let first = bpms[0];
    if bpms.iter().any(|bpm| (bpm - first).abs() > 0.001) {
        return Ok(None);
    }
    Ok(Some(first))
}

pub fn decode_stream(raw: &[u8]) -> Result<Analysis> {
    let mut cursor = 0;
    let mut track: Option<Vec<u8>> = None;
    let mut beat: Option<Vec<u8>> = None;
    let mut overview = Wave::default();
    let mut rgb = Wave::default();
    let mut finished = false;
    while cursor < raw.len() {
        if raw.len() - cursor < 24 {
            bail!("incomplete analyzer frame header");
        }
        let command = u32le(&raw[cursor + 4..cursor + 8])?;
        let len = u64le(&raw[cursor + 8..cursor + 16])?;
        let crc = u32le(&raw[cursor + 16..cursor + 20])?;
        if len > MAX_PACKET as u64 || raw.len() - cursor - 24 < len as usize {
            bail!("incomplete or oversized analyzer frame");
        }
        let payload = &raw[cursor + 24..cursor + 24 + len as usize];
        if crc32fast::hash(payload) != crc {
            bail!("analyzer frame CRC mismatch");
        }
        if finished {
            bail!("packets arrived after worker finished");
        }
        match command {
            6 => {
                beat = Some(payload.to_vec());
            }
            7 => {
                track = Some(payload.to_vec());
            }
            8 => overview.add(payload, false)?,
            9 => rgb.add(payload, true)?,
            14 | 16 => bail!("analyzer stopped or failed"),
            15 => finished = true,
            _ => {}
        }
        cursor += 24 + len as usize;
    }
    if !finished {
        bail!("analyzer did not send finished packet");
    }
    let mut track = track.context("track data missing")?;
    let beat = beat.context("beat data missing")?;
    if track.len() < 44 {
        bail!("track data header truncated");
    }
    let sample_rate = f64be(&track[..8])?;
    let samples = i64be(&track[8..16])?;
    let key = i32::from_be_bytes(track[16..20].try_into()?);
    if !sample_rate.is_finite() || sample_rate <= 0.0 || samples < 0 {
        bail!("invalid analyzer track data");
    }
    let bpm = beat_bpm(&beat)?;
    let (overview_entries, overview_data) = overview.finish()?;
    let (rgb_entries, rgb_sidecar) = rgb.finish()?;
    if overview_entries != rgb_entries {
        bail!("overview and RGB counts disagree");
    }
    let track_data = qcompress(&track)?;
    // Keep raw packet only until validation has completed.
    track.clear();
    Ok(Analysis {
        sample_rate,
        samples,
        bpm,
        key,
        track_data,
        beat_data: qcompress(&beat)?,
        overview_data,
        rgb_sidecar,
        overview_entries,
        rgb_entries,
    })
}
