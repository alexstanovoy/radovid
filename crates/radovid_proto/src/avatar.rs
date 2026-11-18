//! The avatar image format and its codec, shared by the service (which validates and stores what
//! clients upload) and by clients (which decode what other clients uploaded and encode their own).
//!
//! An avatar is a static, non-interlaced PNG of exactly [`AVATAR_SIDE`] by [`AVATAR_SIDE`] pixels
//! with 8-bit samples, at most [`MAX_AVATAR_BYTES`] long, made of nothing but `IHDR`, an optional
//! `PLTE`, an optional `tRNS`, `IDAT` and `IEND`: no text, profile, animation or any other chunk.
//! Its single zlib stream must inflate to exactly the scanlines of the image, so the decoded size
//! is a constant of the format and an image that is small on the wire cannot expand beyond it.
//!
//! Both sides decode through [`decode_avatar`], which checks the whole structure and every
//! checksum, never allocates more than the decoded image and returns an error, never a panic, for
//! any bytes at all.
use crate::{AVATAR_SIDE, Error, MAX_AVATAR_BYTES};
use std::io::Cursor;

mod compress;

pub use compress::encode_avatar_rgba;

const SIDE: usize = AVATAR_SIDE as usize;
/// The most bytes a decoded avatar holds: every pixel with four 8-bit samples.
const MAX_DECODED_BYTES: usize = SIDE * SIDE * 4;
/// What the PNG decoder may allocate besides the image itself, a few times the largest scanline
/// buffer.
const DECODER_MEMORY_BYTES: usize = 4 * MAX_DECODED_BYTES;
const MAX_PALETTE_BYTES: usize = 3 * 256;

/// Why an avatar was refused or could not be made. The wire only says [`Error::BadAvatar`]; the
/// reason is for the log and for the person choosing a picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AvatarError(pub &'static str);

impl core::fmt::Display for AvatarError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "avatar refused: {}", self.0)
    }
}

impl std::error::Error for AvatarError {}

impl From<AvatarError> for Error {
    fn from(_: AvatarError) -> Error {
        Error::BadAvatar
    }
}

/// The refusal of an image that does not fit [`MAX_AVATAR_BYTES`] once encoded.
const TOO_LARGE: AvatarError = AvatarError("the encoded avatar exceeds the byte limit");

/// How the samples of an [`AvatarImage`] are laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    Gray,
    GrayAlpha,
    Rgb,
    Rgba,
    Indexed,
}

impl Layout {
    /// Filtering helps smooth samples, and hurts the indexes of a palette, where a neighbouring
    /// index is no nearer in colour.
    fn filter(self) -> png::Filter {
        match self {
            Layout::Indexed => png::Filter::NoFilter,
            _ => png::Filter::Adaptive,
        }
    }

    fn channels(self) -> usize {
        match self {
            Layout::Gray | Layout::Indexed => 1,
            Layout::GrayAlpha => 2,
            Layout::Rgb => 3,
            Layout::Rgba => 4,
        }
    }

    fn color_type(self) -> png::ColorType {
        match self {
            Layout::Gray => png::ColorType::Grayscale,
            Layout::GrayAlpha => png::ColorType::GrayscaleAlpha,
            Layout::Rgb => png::ColorType::Rgb,
            Layout::Rgba => png::ColorType::Rgba,
            Layout::Indexed => png::ColorType::Indexed,
        }
    }

    fn from_color_type(color: png::ColorType) -> Layout {
        match color {
            png::ColorType::Grayscale => Layout::Gray,
            png::ColorType::GrayscaleAlpha => Layout::GrayAlpha,
            png::ColorType::Rgb => Layout::Rgb,
            png::ColorType::Rgba => Layout::Rgba,
            png::ColorType::Indexed => Layout::Indexed,
        }
    }
}

/// The pixels of a decoded avatar, as the PNG stores them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AvatarImage {
    layout: Layout,
    samples: Vec<u8>,
    palette: Option<Vec<u8>>,
    transparency: Option<Vec<u8>>,
}

/// Checks the supported static image envelope, returning the bounded IDAT bytes and the channels
/// per pixel. Pixel, zlib and checksum validation belong to [`decode_avatar`].
pub fn avatar_image_data(bytes: &[u8]) -> Result<(Vec<u8>, usize), AvatarError> {
    if bytes.len() > MAX_AVATAR_BYTES {
        return Err(AvatarError("more bytes than the avatar limit"));
    }
    if bytes.get(..8) != Some(b"\x89PNG\r\n\x1a\n".as_slice()) {
        return Err(AvatarError("not a PNG"));
    }
    let (kind, header, mut offset) = chunk(bytes, 8)?;
    if kind != b"IHDR" {
        return Err(AvatarError("the first chunk is not IHDR"));
    }
    let (color, channels) = header_shape(header)?;
    let mut state = ImageChunks::default();
    while offset < bytes.len() {
        let (kind, payload, end) = chunk(bytes, offset)?;
        if state.accept(color, kind, payload)? {
            if end != bytes.len() {
                return Err(AvatarError("bytes after IEND"));
            }
            return Ok((state.compressed, channels));
        }
        offset = end;
    }
    Err(AvatarError("no IEND"))
}

fn chunk(bytes: &[u8], offset: usize) -> Result<(&[u8], &[u8], usize), AvatarError> {
    const TRUNCATED: AvatarError = AvatarError("a truncated chunk");
    let length = bytes.get(offset..offset + 4).ok_or(TRUNCATED)?;
    let length = u32::from_be_bytes(length.try_into().map_err(|_| TRUNCATED)?) as usize;
    let end = offset
        .checked_add(12)
        .and_then(|end| end.checked_add(length))
        .filter(|end| *end <= bytes.len())
        .ok_or(TRUNCATED)?;
    Ok((
        &bytes[offset + 4..offset + 8],
        &bytes[offset + 8..end - 4],
        end,
    ))
}

fn header_shape(header: &[u8]) -> Result<(u8, usize), AvatarError> {
    if header.len() != 13 {
        return Err(AvatarError("a malformed IHDR"));
    }
    if header[..4] != AVATAR_SIDE.to_be_bytes() || header[4..8] != AVATAR_SIDE.to_be_bytes() {
        return Err(AvatarError("not exactly the avatar size"));
    }
    if header[8] != 8 {
        return Err(AvatarError("samples are not 8 bits"));
    }
    if header[10..13] != [0, 0, 0] {
        return Err(AvatarError(
            "an unknown compression or filter, or interlacing",
        ));
    }
    let color = header[9];
    let channels = match color {
        0 | 3 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => return Err(AvatarError("an unknown colour type")),
    };
    Ok((color, channels))
}

#[derive(Default)]
struct ImageChunks {
    palette: Option<usize>,
    transparency: bool,
    data: bool,
    compressed: Vec<u8>,
}

impl ImageChunks {
    // True ends the image; an error rejects the chunk rather than silently discarding it.
    fn accept(&mut self, color: u8, kind: &[u8], payload: &[u8]) -> Result<bool, AvatarError> {
        match kind {
            b"PLTE" if self.palette.is_none() && !self.transparency && !self.data => {
                if !matches!(color, 2 | 3 | 6)
                    || payload.is_empty()
                    || payload.len() > MAX_PALETTE_BYTES
                    || !payload.len().is_multiple_of(3)
                {
                    return Err(AvatarError("a malformed PLTE"));
                }
                self.palette = Some(payload.len() / 3);
            }
            b"tRNS" if !self.transparency && !self.data => {
                if !valid_transparency(color, self.palette, payload) {
                    return Err(AvatarError("a malformed tRNS"));
                }
                self.transparency = true;
            }
            b"IDAT" if color != 3 || self.palette.is_some() => {
                self.data = true;
                self.compressed.extend_from_slice(payload);
            }
            b"IEND" if self.data && payload.is_empty() => return Ok(true),
            _ => return Err(AvatarError("a chunk the avatar format does not allow")),
        }
        Ok(false)
    }
}

fn valid_transparency(color: u8, palette: Option<usize>, payload: &[u8]) -> bool {
    match color {
        0 => payload.len() == 2 && payload[0] == 0,
        2 => {
            payload.len() == 6
                && payload
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .all(|sample| sample[0] == 0)
        }
        3 => !payload.is_empty() && palette.is_some_and(|entries| payload.len() <= entries),
        _ => false,
    }
}

/// Checks `bytes` entirely and returns its pixels.
pub fn decode_avatar(bytes: &[u8]) -> Result<AvatarImage, AvatarError> {
    let (compressed, channels) = avatar_image_data(bytes)?;
    // A tiny compressed stream may advertise arbitrary inflated data. Require exactly the
    // scanlines, the checksum and the end of the one zlib stream before pixel decoding; one byte
    // of room shows an excess without letting it grow.
    let expected = SIDE * (1 + SIDE * channels);
    let mut scanlines = vec![0; expected + 1];
    let mut inflate = flate2::Decompress::new(true);
    let status = inflate
        .decompress(&compressed, &mut scanlines, flate2::FlushDecompress::Finish)
        .map_err(|_| AvatarError("the pixel data is not a zlib stream"))?;
    if status != flate2::Status::StreamEnd
        || inflate.total_out() != expected as u64
        || inflate.total_in() != compressed.len() as u64
    {
        return Err(AvatarError(
            "the pixel data does not inflate to exactly the image",
        ));
    }
    drop(scanlines);
    let mut options = png::DecodeOptions::default();
    options.set_ignore_checksums(false);
    // PNG decoders normally discard ancillary chunks with a bad CRC; avatars must refuse them.
    options.set_skip_ancillary_crc_failures(false);
    let mut decoder = png::Decoder::new_with_options(Cursor::new(bytes), options);
    decoder.set_limits(png::Limits {
        bytes: DECODER_MEMORY_BYTES,
    });
    let mut reader = decoder
        .read_info()
        .map_err(|_| AvatarError("the PNG decoder refused the header"))?;
    let mut samples = vec![0; SIDE * SIDE * channels];
    if reader.output_buffer_size() != Some(samples.len()) {
        return Err(AvatarError("the decoded size is not the avatar's"));
    }
    let palette_entries = reader
        .info()
        .palette
        .as_ref()
        .map(|palette| palette.len() / 3);
    let output = reader
        .next_frame(&mut samples)
        .map_err(|_| AvatarError("the PNG decoder refused the pixels"))?;
    let layout = Layout::from_color_type(output.color_type);
    if layout == Layout::Indexed
        && palette_entries
            .is_none_or(|entries| samples.iter().any(|index| usize::from(*index) >= entries))
    {
        return Err(AvatarError("a pixel names a missing palette entry"));
    }
    reader
        .finish()
        .map_err(|_| AvatarError("the PNG decoder refused the end of the file"))?;
    Ok(AvatarImage {
        layout,
        samples,
        palette: reader.info().palette.as_ref().map(|bytes| bytes.to_vec()),
        transparency: reader.info().trns.as_ref().map(|bytes| bytes.to_vec()),
    })
}

/// Whether `bytes` is a valid avatar.
pub fn validate_avatar(bytes: &[u8]) -> Result<(), AvatarError> {
    decode_avatar(bytes).map(|_| ())
}

/// Only decoded pixel data and pixel transparency enter the fixed encoder, so that nothing else a
/// peer hid in the file (unused palette entries, the choices of a deflate encoder) is kept.
/// Canonical bytes are stable: canonicalising them again returns them.
pub fn canonical_avatar(bytes: &[u8]) -> Result<Vec<u8>, AvatarError> {
    decode_avatar(bytes)?.canonical_png()
}

impl AvatarImage {
    /// The pixels as 8-bit RGBA, [`AVATAR_SIDE`] squared of them, row by row.
    #[must_use]
    pub fn to_rgba(&self) -> Vec<u8> {
        let mut rgba = Vec::with_capacity(MAX_DECODED_BYTES);
        let transparent = self.transparency.as_deref();
        match self.layout {
            Layout::Gray => {
                for value in &self.samples {
                    let alpha = if transparent == Some(&[*value]) {
                        0
                    } else {
                        255
                    };
                    rgba.extend_from_slice(&[*value, *value, *value, alpha]);
                }
            }
            Layout::GrayAlpha => {
                for pixel in self.samples.as_chunks::<2>().0 {
                    rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]);
                }
            }
            Layout::Rgb => {
                for pixel in self.samples.as_chunks::<3>().0 {
                    let alpha = if transparent == Some(pixel.as_slice()) {
                        0
                    } else {
                        255
                    };
                    rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], alpha]);
                }
            }
            Layout::Rgba => rgba.extend_from_slice(&self.samples),
            Layout::Indexed => {
                let palette = self.palette.as_deref().unwrap_or_default();
                for index in &self.samples {
                    let at = usize::from(*index);
                    let color = palette.get(at * 3..at * 3 + 3).unwrap_or(&[0, 0, 0]);
                    let alpha = transparent.and_then(|alphas| alphas.get(at)).copied();
                    rgba.extend_from_slice(&[color[0], color[1], color[2], alpha.unwrap_or(255)]);
                }
            }
        }
        rgba
    }

    /// The one byte sequence this image is stored as.
    pub(crate) fn canonical_png(&self) -> Result<Vec<u8>, AvatarError> {
        let mut image = self.clone();
        if image.layout == Layout::Indexed {
            image.compact_palette()?;
        }
        let mut output = BoundedPng(Vec::with_capacity(MAX_AVATAR_BYTES));
        {
            let mut encoder = png::Encoder::new(&mut output, AVATAR_SIDE, AVATAR_SIDE);
            encoder.set_color(image.layout.color_type());
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_deflate_compression(png::DeflateCompression::Level(9));
            encoder.set_filter(image.layout.filter());
            if image.layout == Layout::Indexed {
                encoder.set_palette(
                    image
                        .palette
                        .ok_or(AvatarError("an indexed image without a palette"))?,
                );
            }
            if let Some(transparency) = image.transparency {
                encoder.set_trns(encoder_transparency(image.layout, transparency)?);
            }
            let mut writer = encoder.write_header().map_err(|_| TOO_LARGE)?;
            writer
                .write_image_data(&image.samples)
                .map_err(|_| TOO_LARGE)?;
            writer.finish().map_err(|_| TOO_LARGE)?;
        }
        validate_avatar(&output.0)?;
        Ok(output.0)
    }

    /// Drops the palette entries no pixel uses and orders the rest by first use; the transparency
    /// of the dropped entries goes with them.
    fn compact_palette(&mut self) -> Result<(), AvatarError> {
        const BROKEN: AvatarError = AvatarError("an indexed image without its palette");
        let palette = self.palette.take().ok_or(BROKEN)?;
        let transparency = self.transparency.take().unwrap_or_default();
        let mut positions = [None; 256];
        let mut colors = Vec::new();
        let mut alpha = Vec::new();
        for pixel in &mut self.samples {
            let old = usize::from(*pixel);
            let next = match positions[old] {
                Some(index) => index,
                None => {
                    let next = u8::try_from(colors.len() / 3).map_err(|_| BROKEN)?;
                    colors.extend_from_slice(palette.get(old * 3..old * 3 + 3).ok_or(BROKEN)?);
                    alpha.push(transparency.get(old).copied().unwrap_or(255));
                    positions[old] = Some(next);
                    next
                }
            };
            *pixel = next;
        }
        while alpha.last() == Some(&255) {
            alpha.pop();
        }
        self.palette = Some(colors);
        self.transparency = (!alpha.is_empty()).then_some(alpha);
        Ok(())
    }
}

fn encoder_transparency(layout: Layout, samples: Vec<u8>) -> Result<Vec<u8>, AvatarError> {
    match layout {
        Layout::Gray | Layout::Rgb => {
            if samples.len() != layout.channels() {
                return Err(AvatarError("a transparent colour of the wrong size"));
            }
            // The decoder reduces these PNG 16-bit sample fields to our validated 8-bit values.
            Ok(samples.into_iter().flat_map(|value| [0, value]).collect())
        }
        Layout::Indexed => Ok(samples),
        Layout::GrayAlpha | Layout::Rgba => {
            Err(AvatarError("transparency beside an alpha channel"))
        }
    }
}

/// An output that refuses to grow past the avatar limit.
struct BoundedPng(Vec<u8>);

impl std::io::Write for BoundedPng {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_AVATAR_BYTES.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other("encoded avatar exceeds byte limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
