//! Bounded PNG framing shared by service validation and client decoder admission.
use crate::{AVATAR_SIDE, MAX_AVATAR_BYTES};

/// Checks the supported static image envelope, returning bounded IDAT bytes and channels.
/// Full pixel, zlib and checksum validation belongs to the service decoder.
pub fn avatar_image_data(bytes: &[u8]) -> Option<(Vec<u8>, u64)> {
    if bytes.len() > MAX_AVATAR_BYTES || bytes.get(..8)? != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let (kind, header, mut offset) = chunk(bytes, 8)?;
    if kind != b"IHDR" { return None; }
    let (color, channels) = header_shape(header)?;
    let mut state = ImageChunks::default();
    while offset < bytes.len() {
        let (kind, payload, end) = chunk(bytes, offset)?;
        if state.accept(color, kind, payload)? {
            return (end == bytes.len()).then_some((state.compressed, channels));
        }
        offset = end;
    }
    None
}

fn chunk(bytes: &[u8], offset: usize) -> Option<(&[u8], &[u8], usize)> {
    let length = u32::from_be_bytes(bytes.get(offset..offset+4)?.try_into().ok()?) as usize;
    let end = offset.checked_add(12)?.checked_add(length)?;
    bytes.get(..end)?;
    Some((bytes.get(offset+4..offset+8)?, bytes.get(offset+8..end-4)?, end))
}

fn header_shape(header: &[u8]) -> Option<(u8, u64)> {
    if header.len() != 13 || header[..4] != AVATAR_SIDE.to_be_bytes()
        || header[4..8] != AVATAR_SIDE.to_be_bytes() || header[8] != 8
        || header[10..13] != [0,0,0] { return None; }
    let color = header[9];
    let channels = match color { 0|3 => 1, 2 => 3, 4 => 2, 6 => 4, _ => return None };
    Some((color, channels))
}

#[derive(Default)]
struct ImageChunks {
    palette: Option<usize>,
    transparency: bool,
    data: bool,
    compressed: Vec<u8>,
}

impl ImageChunks {
    // True ends the image; None rejects the chunk rather than silently discarding it.
    fn accept(&mut self, color: u8, kind: &[u8], payload: &[u8]) -> Option<bool> {
        match kind {
            b"PLTE" if self.palette.is_none() && !self.transparency && !self.data => {
                if !matches!(color,2|3|6) || payload.is_empty() || payload.len()>768
                    || !payload.len().is_multiple_of(3) { return None; }
                self.palette=Some(payload.len()/3);
            }
            b"tRNS" if !self.transparency && !self.data => {
                if !valid_transparency(color,self.palette,payload) { return None; }
                self.transparency=true;
            }
            b"IDAT" if color!=3 || self.palette.is_some() => {
                self.data=true;
                self.compressed.extend_from_slice(payload);
            }
            b"IEND" if self.data && payload.is_empty() => return Some(true),
            _ => return None,
        }
        Some(false)
    }
}

fn valid_transparency(color: u8, palette: Option<usize>, payload: &[u8]) -> bool {
    match color {
        0 => payload.len()==2 && payload[0]==0,
        2 => payload.len()==6 && payload.as_chunks::<2>().0.iter().all(|sample|sample[0]==0),
        3 => !payload.is_empty() && palette.is_some_and(|entries|payload.len()<=entries),
        _ => false,
    }
}
