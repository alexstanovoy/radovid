//! What the fuzz targets share: how an input is cut into frames.

/// The pieces of an input: each a little endian `u16` length and that many
/// bytes, the last one cut short by the input's end.
pub fn frames(mut data: &[u8]) -> impl Iterator<Item = &[u8]> {
    std::iter::from_fn(move || {
        let (prefix, rest) = data.split_first_chunk::<2>()?;
        let len = usize::from(u16::from_le_bytes(*prefix)).min(rest.len());
        let (frame, rest) = rest.split_at(len);
        data = rest;
        Some(frame)
    })
}
