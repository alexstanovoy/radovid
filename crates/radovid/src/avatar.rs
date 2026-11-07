//! The fixed-size public avatar format. No paths, URLs, metadata or animation.
use radovid_proto::{Error, AVATAR_SIDE, avatar_image_data};
use std::io::Cursor;

pub fn validate(bytes: &[u8]) -> Result<(), Error> {
    let (compressed, channels) = avatar_image_data(bytes).ok_or(Error::BadAvatar)?;
    // A tiny compressed stream may advertise arbitrary inflated data. Require exactly
    // the scanlines, the checksum and the end of the one zlib stream before pixel decoding.
    let mut scanlines = [0; 4129];
    let mut inflate = flate2::Decompress::new(true);
    let status = inflate.decompress(&compressed, &mut scanlines, flate2::FlushDecompress::Finish)
        .map_err(|_| Error::BadAvatar)?;
    let expected = u64::from(AVATAR_SIDE) * (1 + u64::from(AVATAR_SIDE) * channels);
    if status != flate2::Status::StreamEnd || inflate.total_out() != expected
        || inflate.total_in() != compressed.len() as u64 {
        return Err(Error::BadAvatar);
    }
    let mut decoder = png::Decoder::new_with_limits(Cursor::new(bytes), png::Limits { bytes: 64 * 1024 });
    decoder.ignore_checksums(false);
    let mut reader = decoder.read_info().map_err(|_| Error::BadAvatar)?;
    let mut pixels = [0; 4096];
    if reader.output_buffer_size().is_none_or(|size| size > pixels.len()) {
        return Err(Error::BadAvatar);
    }
    let palette_entries = reader.info().palette.as_ref().map(|palette| palette.len()/3);
    let output = reader.next_frame(&mut pixels).map_err(|_| Error::BadAvatar)?;
    if output.color_type == png::ColorType::Indexed && palette_entries.is_none_or(|entries|
        pixels[..output.buffer_size()].iter().any(|index| usize::from(*index)>=entries)) {
        return Err(Error::BadAvatar);
    }
    reader.finish().map_err(|_| Error::BadAvatar)?;
    Ok(())
}


#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use radovid_proto::MAX_AVATAR_BYTES;

    pub(crate) fn image(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("header");
            writer.write_image_data(&vec![128; width as usize * height as usize * 4]).expect("pixels");
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
        let bytes = image(32,32);
        for end in 0..bytes.len() {
            assert_eq!(validate(&bytes[..end]), Err(Error::BadAvatar), "cut at {end}");
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
        let mut zip = flate2::write::ZlibEncoder::new(Vec::new(),flate2::Compression::best());
        zip.write_all(raw).expect("compress");
        let compressed = zip.finish().expect("zlib");
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes,32,32);
            encoder.set_color(png::ColorType::Rgba);
            let mut writer = encoder.write_header().expect("header");
            if let Some(kind) = extra { writer.write_chunk(kind,b"metadata").expect("extra"); }
            writer.write_chunk(png::chunk::IDAT,&compressed).expect("compressed pixels");
            // The manually supplied IDAT is deliberate: normal encoders refuse malformed scanlines.
            // Drop writes IEND; write_chunk alone does not mark it as written.
        }
        bytes
    }

    #[test]
    fn avatar_rejects_metadata_animation_and_inflate_bombs_before_pixel_decoding() {
        let raw = vec![0;32*(1+32*4)];
        assert_eq!(validate(&raw_image(&raw,None)),Ok(()));
        for kind in [*b"tEXt",*b"iCCP",*b"acTL",*b"fdAT"] {
            assert_eq!(validate(&raw_image(&raw,Some(png::chunk::ChunkType(kind)))),Err(Error::BadAvatar));
        }
        let bomb = raw_image(&vec![0;100_000],None);
        assert!(bomb.len() <= MAX_AVATAR_BYTES);
        assert_eq!(validate(&bomb),Err(Error::BadAvatar));
        assert_eq!(validate(&raw_image(&raw[..raw.len()-1],None)),Err(Error::BadAvatar));
        let mut bad_filter = raw;
        bad_filter[0]=5;
        assert_eq!(validate(&raw_image(&bad_filter,None)),Err(Error::BadAvatar));
    }

    #[test]
    fn avatar_accepts_palette_transparency_with_the_same_pixel_bounds() {
        let mut bytes=Vec::new();
        {
            let mut encoder=png::Encoder::new(&mut bytes,32,32);
            encoder.set_color(png::ColorType::Indexed);
            encoder.set_palette(vec![255,0,0,0,0,255]);
            encoder.set_trns(vec![0,255]);
            encoder.write_header().expect("header").write_image_data(&vec![1;1024]).expect("pixels");
        }
        assert!(bytes.len() <= MAX_AVATAR_BYTES);
        assert_eq!(validate(&bytes),Ok(()));
    }

    #[test]
    fn avatar_refuses_palette_indices_without_a_palette_entry() {
        let mut bytes=Vec::new();
        {
            let mut encoder=png::Encoder::new(&mut bytes,32,32);
            encoder.set_color(png::ColorType::Indexed);
            encoder.set_palette(vec![255,0,0]);
            encoder.write_header().expect("header").write_image_data(&vec![1;1024]).expect("pixels");
        }
        assert_eq!(validate(&bytes),Err(Error::BadAvatar));
    }
}
