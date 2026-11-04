use flate2::read::ZlibDecoder;
use rekordbox_to_engine::anlz::convert_cues;
use std::io::Read;

fn section(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = tag.to_vec();
    out.extend_from_slice(&12u32.to_be_bytes());
    out.extend_from_slice(&(12u32 + payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

fn file(section: &[u8]) -> Vec<u8> {
    let mut out = b"PMAI".to_vec();
    out.extend_from_slice(&12u32.to_be_bytes());
    out.extend_from_slice(&(12u32 + section.len() as u32).to_be_bytes());
    out.extend_from_slice(section);
    out
}

#[test]
fn converts_hot_cue_to_engine_sample_offset() {
    let mut entry = vec![0u8; 40];
    entry[..4].copy_from_slice(b"PCP2");
    entry[8..12].copy_from_slice(&40u32.to_be_bytes());
    entry[12..16].copy_from_slice(&1u32.to_be_bytes());
    entry[16] = 1;
    entry[20..24].copy_from_slice(&2000u32.to_be_bytes());
    let data = file(&section(b"PCO2", &entry));
    let result = convert_cues(&[data], 44_100).unwrap();
    let cell = result.quick_cues.unwrap();
    let mut raw = Vec::new();
    ZlibDecoder::new(&cell[4..]).read_to_end(&mut raw).unwrap();
    assert_eq!(i64::from_be_bytes(raw[..8].try_into().unwrap()), 8);
    assert_eq!(f64::from_be_bytes(raw[9..17].try_into().unwrap()), 88_200.0);
}
