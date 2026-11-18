//! What the service holds an avatar to: the shared format of `radovid_proto`, refused with the
//! wire's one `BadAvatar` however it fails. The service never resizes, converts or repairs a
//! picture: it accepts exactly the format, or it refuses.
use radovid_proto::{Error, canonical_avatar, validate_avatar};

pub fn validate(bytes: &[u8]) -> Result<(), Error> {
    Ok(validate_avatar(bytes)?)
}

/// The one byte sequence a valid avatar is stored as; a client that sends these bytes gets back
/// exactly them.
pub fn canonicalize(bytes: &[u8]) -> Result<Vec<u8>, Error> {
    Ok(canonical_avatar(bytes)?)
}

#[cfg(test)]
pub(crate) mod tests {
    use radovid_proto::AVATAR_SIDE;

    /// A flat picture of the given size, encoded as an ordinary PNG encoder would.
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

    /// An avatar-sized picture of one colour, in the bytes the service stores it as.
    pub(crate) fn avatar() -> Vec<u8> {
        super::canonicalize(&image(AVATAR_SIDE, AVATAR_SIDE)).expect("a valid avatar")
    }

    /// An avatar-sized picture with a hand-written chunk in front of its pixels.
    pub(crate) fn image_with_chunks(
        color: png::ColorType,
        chunks: &[(png::chunk::ChunkType, &[u8])],
    ) -> Vec<u8> {
        use std::io::Write;
        let side = AVATAR_SIDE as usize;
        let mut zip = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        zip.write_all(&vec![0; side * (1 + side * color.samples())])
            .expect("scanlines");
        let compressed = zip.finish().expect("zlib");
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, AVATAR_SIDE, AVATAR_SIDE);
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
}
