//! The fixed-size public avatar format. No paths, URLs, metadata or animation.
use radovid_proto::{AVATAR_SIDE, Error, avatar_image_data};
use std::io::Cursor;

const PIXEL_CAP: usize = AVATAR_SIDE as usize * AVATAR_SIDE as usize * 4;
const SCANLINE_CAP: usize = PIXEL_CAP + AVATAR_SIDE as usize;

struct Pixels {
    bytes:[u8;PIXEL_CAP],
    size:usize,
    color:png::ColorType,
    palette:Option<Vec<u8>>,
    transparency:Option<Vec<u8>>,
}

pub fn validate(bytes:&[u8])->Result<(),Error> {decode(bytes).map(|_|())}

fn decode(bytes: &[u8]) -> Result<Pixels, Error> {
    let (compressed, channels) = avatar_image_data(bytes).ok_or(Error::BadAvatar)?;
    // A tiny compressed stream may advertise arbitrary inflated data. Require exactly
    // the scanlines, the checksum and the end of the one zlib stream before pixel decoding.
    let mut scanlines = [0; SCANLINE_CAP + 1];
    let mut inflate = flate2::Decompress::new(true);
    let status = inflate
        .decompress(&compressed, &mut scanlines, flate2::FlushDecompress::Finish)
        .map_err(|_| Error::BadAvatar)?;
    let expected = u64::from(AVATAR_SIDE) * (1 + u64::from(AVATAR_SIDE) * channels);
    if status != flate2::Status::StreamEnd
        || inflate.total_out() != expected
        || inflate.total_in() != compressed.len() as u64
    {
        return Err(Error::BadAvatar);
    }
    let mut options = png::DecodeOptions::default();
    options.set_ignore_checksums(false);
    // PNG decoders normally discard ancillary chunks with a bad CRC; avatars must refuse them.
    options.set_skip_ancillary_crc_failures(false);
    let mut decoder = png::Decoder::new_with_options(Cursor::new(bytes), options);
    decoder.set_limits(png::Limits { bytes: 64 * 1024 });
    let mut reader = decoder.read_info().map_err(|_| Error::BadAvatar)?;
    let mut pixels = [0; PIXEL_CAP];
    if reader
        .output_buffer_size()
        .is_none_or(|size| size > pixels.len())
    {
        return Err(Error::BadAvatar);
    }
    let palette_entries = reader
        .info()
        .palette
        .as_ref()
        .map(|palette| palette.len() / 3);
    let output = reader
        .next_frame(&mut pixels)
        .map_err(|_| Error::BadAvatar)?;
    if output.color_type == png::ColorType::Indexed
        && palette_entries.is_none_or(|entries| {
            pixels[..output.buffer_size()]
                .iter()
                .any(|index| usize::from(*index) >= entries)
        })
    {
        return Err(Error::BadAvatar);
    }
    reader.finish().map_err(|_| Error::BadAvatar)?;
    Ok(Pixels{bytes:pixels,size:output.buffer_size(),color:output.color_type,
        palette:reader.info().palette.as_ref().map(|bytes|bytes.to_vec()),
        transparency:reader.info().trns.as_ref().map(|bytes|bytes.to_vec())})
}

/// Only decoded pixel data and pixel transparency enter the fixed encoder.
pub fn canonicalize(bytes:&[u8])->Result<Vec<u8>,Error> {
    let decoded=decode(bytes)?;
    let mut output=BoundedPng(Vec::with_capacity(radovid_proto::MAX_AVATAR_BYTES));
    {
        let mut encoder=png::Encoder::new(&mut output,AVATAR_SIDE,AVATAR_SIDE);
        encoder.set_color(decoded.color);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Balanced);
        encoder.set_filter(png::Filter::Adaptive);
        if decoded.color==png::ColorType::Indexed {
            encoder.set_palette(decoded.palette.ok_or(Error::BadAvatar)?);
        }
        if let Some(transparency)=decoded.transparency {encoder.set_trns(transparency);}
        let mut writer=encoder.write_header().map_err(|_|Error::BadAvatar)?;
        writer.write_image_data(&decoded.bytes[..decoded.size]).map_err(|_|Error::BadAvatar)?;
        writer.finish().map_err(|_|Error::BadAvatar)?;
    }
    Ok(output.0)
}

struct BoundedPng(Vec<u8>);
impl std::io::Write for BoundedPng {
    fn write(&mut self,bytes:&[u8])->std::io::Result<usize> {
        if bytes.len()>radovid_proto::MAX_AVATAR_BYTES.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other("encoded avatar exceeds byte limit"));
        }
        self.0.extend_from_slice(bytes);Ok(bytes.len())
    }
    fn flush(&mut self)->std::io::Result<()> {Ok(())}
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use radovid_proto::MAX_AVATAR_BYTES;

    type ChunkData<'a> = (png::chunk::ChunkType, &'a [u8]);
    type ChunkCase<'a> = (png::ColorType, &'a [ChunkData<'a>]);

    pub(crate) fn image_with_chunks(color: png::ColorType, chunks: &[ChunkData<'_>]) -> Vec<u8> {
        use std::io::Write;
        let mut zip = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        zip.write_all(&vec![0; 32 * (1 + 32 * color.samples())])
            .expect("scanlines");
        let compressed = zip.finish().expect("zlib");
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 32, 32);
            encoder.set_color(color);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("header");
            for (kind, data) in chunks {
                writer
                    .write_chunk(*kind, data)
                    .expect("chunk with valid CRC");
            }
            writer
                .write_chunk(png::chunk::IDAT, &compressed)
                .expect("pixels");
        }
        bytes
    }


    #[test]
    fn reencoding_preserves_nonuniform_pixels_and_is_idempotent() {
        let pixels:Vec<u8>=(0..32*32).flat_map(|index|if (index/32+index%32)%2==0 {[255,1,128]}else{[2,254,77]}).collect();
        let mut original=Vec::new();
        {
            let mut encoder=png::Encoder::new(&mut original,32,32);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_compression(png::Compression::High);
            let mut writer=encoder.write_header().expect("header");
            writer.write_image_data(&pixels).expect("pixels");
        }
        assert!(original.len()<=MAX_AVATAR_BYTES);
        let canonical=canonicalize(&original).expect("canonical");
        assert_eq!(canonicalize(&canonical).expect("idempotent"),canonical);
        let mut reader=png::Decoder::new(Cursor::new(&canonical)).read_info().expect("PNG");
        let mut decoded=[0;PIXEL_CAP];
        let output=reader.next_frame(&mut decoded).expect("decoded pixels");
        assert_eq!(&decoded[..output.buffer_size()],pixels);
    }

    #[test]
    fn avatar_rejects_semantically_invalid_palette_and_transparency_chunks() {
        use png::ColorType::*;
        use png::chunk::{PLTE, tRNS};
        let cases: &[ChunkCase<'_>] = &[
            (Grayscale, &[(PLTE, &[0, 0, 0])]),
            (GrayscaleAlpha, &[(PLTE, &[0, 0, 0])]),
            (Rgb, &[(PLTE, &[0, 0, 0, 0])]),
            (Rgba, &[(tRNS, &[0, 0])]),
            (Grayscale, &[(tRNS, &[0])]),
            (Grayscale, &[(tRNS, &[1, 0])]),
            (Rgb, &[(tRNS, &[0, 0])]),
            (Rgb, &[(tRNS, &[1, 0, 0, 0, 0, 0])]),
            (Indexed, &[(PLTE, &[0, 0, 0]), (tRNS, &[0, 128])]),
            (Indexed, &[(tRNS, &[0]), (PLTE, &[0, 0, 0])]),
        ];
        let accepted: Vec<_> = cases
            .iter()
            .enumerate()
            .filter_map(|(index, (color, chunks))| {
                validate(&image_with_chunks(*color, chunks))
                    .is_ok()
                    .then_some(index)
            })
            .collect();
        assert!(
            accepted.is_empty(),
            "invalid PNG chunk cases accepted: {accepted:?}"
        );
    }

    #[test]
    fn avatar_accepts_supported_color_and_transparency_combinations() {
        use png::ColorType::*;
        use png::chunk::{PLTE, tRNS};
        let cases: &[ChunkCase<'_>] = &[
            (Grayscale, &[(tRNS, &[0, 128])]),
            (GrayscaleAlpha, &[]),
            (Rgb, &[(tRNS, &[0, 1, 0, 2, 0, 3])]),
            (Rgb, &[(PLTE, &[1, 2, 3]), (tRNS, &[0, 1, 0, 2, 0, 3])]),
            (Rgba, &[(PLTE, &[1, 2, 3])]),
            (Indexed, &[(PLTE, &[1, 2, 3]), (tRNS, &[128])]),
        ];
        for (color, chunks) in cases {
            assert_eq!(
                validate(&image_with_chunks(*color, chunks)),
                Ok(()),
                "{color:?}"
            );
        }
    }

    #[test]
    fn avatar_rejects_a_transparency_chunk_with_a_bad_crc() {
        let mut bytes = image_with_chunks(
            png::ColorType::Indexed,
            &[(png::chunk::PLTE, &[0, 0, 0]), (png::chunk::tRNS, &[128])],
        );
        assert_eq!(validate(&bytes), Ok(()));
        let mut offset = 8;
        while offset < bytes.len() {
            let length =
                u32::from_be_bytes(bytes[offset..offset + 4].try_into().expect("chunk length"))
                    as usize;
            if &bytes[offset + 4..offset + 8] == b"tRNS" {
                bytes[offset + 8 + length] ^= 1;
                break;
            }
            offset += 12 + length;
        }
        assert_eq!(validate(&bytes), Err(Error::BadAvatar));
    }

    pub(crate) fn image(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("header");
            writer
                .write_image_data(&vec![128; width as usize * height as usize * 4])
                .expect("pixels");
        }
        bytes
    }

    #[test]
    fn avatar_accepts_a_complete_small_png_and_refuses_wrong_dimensions_or_size() {
        let bytes = image(32, 32);
        assert!(bytes.len() <= 512);
        assert_eq!(validate(&bytes), Ok(()));
        assert_eq!(validate(&image(31, 32)), Err(Error::BadAvatar));
        assert_eq!(validate(&vec![0; 513]), Err(Error::BadAvatar));
    }

    #[test]
    fn avatar_refuses_non_images_truncation_trailing_bytes_and_bad_checksums() {
        assert_eq!(validate(b"<svg onload='alert(1)'/>"), Err(Error::BadAvatar));
        let bytes = image(32, 32);
        for end in 0..bytes.len() {
            assert_eq!(
                validate(&bytes[..end]),
                Err(Error::BadAvatar),
                "cut at {end}"
            );
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(validate(&trailing), Err(Error::BadAvatar));
        let mut corrupt = bytes;
        corrupt[29] ^= 1;
        assert_eq!(validate(&corrupt), Err(Error::BadAvatar));
    }

    fn raw_image(raw: &[u8], extra: Option<png::chunk::ChunkType>) -> Vec<u8> {
        use std::io::Write;
        let mut zip = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        zip.write_all(raw).expect("compress");
        let compressed = zip.finish().expect("zlib");
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 32, 32);
            encoder.set_color(png::ColorType::Rgba);
            let mut writer = encoder.write_header().expect("header");
            if let Some(kind) = extra {
                writer.write_chunk(kind, b"metadata").expect("extra");
            }
            writer
                .write_chunk(png::chunk::IDAT, &compressed)
                .expect("compressed pixels");
            // The manually supplied IDAT is deliberate: normal encoders refuse malformed scanlines.
            // Drop writes IEND; write_chunk alone does not mark it as written.
        }
        bytes
    }

    #[test]
    fn avatar_rejects_metadata_animation_and_inflate_bombs_before_pixel_decoding() {
        let raw = vec![0; 32 * (1 + 32 * 4)];
        assert_eq!(validate(&raw_image(&raw, None)), Ok(()));
        for kind in [*b"tEXt", *b"iCCP", *b"acTL", *b"fdAT"] {
            assert_eq!(
                validate(&raw_image(&raw, Some(png::chunk::ChunkType(kind)))),
                Err(Error::BadAvatar)
            );
        }
        let bomb = raw_image(&vec![0; 100_000], None);
        assert!(bomb.len() <= MAX_AVATAR_BYTES);
        assert_eq!(validate(&bomb), Err(Error::BadAvatar));
        assert_eq!(
            validate(&raw_image(&raw[..raw.len() - 1], None)),
            Err(Error::BadAvatar)
        );
        let mut bad_filter = raw;
        bad_filter[0] = 5;
        assert_eq!(
            validate(&raw_image(&bad_filter, None)),
            Err(Error::BadAvatar)
        );
    }

    #[test]
    fn avatar_accepts_palette_transparency_with_the_same_pixel_bounds() {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 32, 32);
            encoder.set_color(png::ColorType::Indexed);
            encoder.set_palette(vec![255, 0, 0, 0, 0, 255]);
            encoder.set_trns(vec![0, 255]);
            encoder
                .write_header()
                .expect("header")
                .write_image_data(&vec![1; 1024])
                .expect("pixels");
        }
        assert!(bytes.len() <= MAX_AVATAR_BYTES);
        assert_eq!(validate(&bytes), Ok(()));
    }

    #[test]
    fn avatar_refuses_palette_indices_without_a_palette_entry() {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 32, 32);
            encoder.set_color(png::ColorType::Indexed);
            encoder.set_palette(vec![255, 0, 0]);
            encoder
                .write_header()
                .expect("header")
                .write_image_data(&vec![1; 1024])
                .expect("pixels");
        }
        assert_eq!(validate(&bytes), Err(Error::BadAvatar));
    }
}
