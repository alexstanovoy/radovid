use super::*;
use std::io::Write;

type ChunkData<'a> = (png::chunk::ChunkType, &'a [u8]);
type ChunkCase<'a> = (png::ColorType, &'a [ChunkData<'a>]);

const SIDE_U32: u32 = AVATAR_SIDE;

fn compressed(raw: &[u8]) -> Vec<u8> {
    let mut zip = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    zip.write_all(raw).expect("compress");
    zip.finish().expect("zlib")
}

/// A flat image of `color` whose chunks are written by hand, in the given order before the data.
fn image_with_chunks(color: png::ColorType, chunks: &[ChunkData<'_>]) -> Vec<u8> {
    let pixels = compressed(&vec![0; SIDE * (1 + SIDE * color.samples())]);
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, SIDE_U32, SIDE_U32);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("header");
        for (kind, data) in chunks {
            writer
                .write_chunk(*kind, data)
                .expect("chunk with valid CRC");
        }
        writer
            .write_chunk(png::chunk::IDAT, &pixels)
            .expect("pixels");
    }
    bytes
}

/// Pixels of `color` encoded by the PNG crate as an ordinary encoder would.
fn encoded(color: png::ColorType, pixels: &[u8], palette: Option<(Vec<u8>, Vec<u8>)>) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, SIDE_U32, SIDE_U32);
        encoder.set_color(color);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::High);
        if let Some((colors, alpha)) = palette {
            encoder.set_palette(colors);
            if !alpha.is_empty() {
                encoder.set_trns(alpha);
            }
        }
        let mut writer = encoder.write_header().expect("header");
        writer.write_image_data(pixels).expect("pixels");
    }
    bytes
}

fn flat(width: u32, height: u32) -> Vec<u8> {
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

fn refusal(bytes: &[u8]) -> &'static str {
    decode_avatar(bytes)
        .expect_err("the avatar must be refused")
        .0
}

#[test]
fn a_complete_png_of_the_avatar_size_decodes_and_any_other_size_is_refused() {
    let bytes = flat(SIDE_U32, SIDE_U32);
    assert!(bytes.len() <= MAX_AVATAR_BYTES);
    assert_eq!(validate_avatar(&bytes), Ok(()));
    for (width, height) in [(31, 256), (256, 255), (32, 32), (257, 257), (512, 128)] {
        assert_eq!(
            refusal(&flat(width, height)),
            "not exactly the avatar size",
            "{width}x{height}"
        );
    }
}

#[test]
fn bytes_beyond_the_limit_are_refused_before_any_parsing() {
    assert_eq!(
        refusal(&vec![0; MAX_AVATAR_BYTES + 1]),
        "more bytes than the avatar limit"
    );
}

#[test]
fn non_images_truncation_trailing_bytes_and_bad_checksums_are_refused() {
    assert_eq!(refusal(b"<svg onload='alert(1)'/>"), "not a PNG");
    let bytes = flat(SIDE_U32, SIDE_U32);
    for end in 0..bytes.len() {
        assert!(decode_avatar(&bytes[..end]).is_err(), "cut at {end}");
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert_eq!(refusal(&trailing), "bytes after IEND");
    let mut corrupt = bytes;
    corrupt[29] ^= 1;
    assert_eq!(refusal(&corrupt), "the PNG decoder refused the header");
}

#[test]
fn text_profile_animation_and_unknown_chunks_are_refused_wherever_they_sit() {
    let raw = vec![0; SIDE * (1 + SIDE * 4)];
    for kind in [
        *b"tEXt", *b"iCCP", *b"acTL", *b"fdAT", *b"eXIf", *b"zTXt", *b"zzzz",
    ] {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, SIDE_U32, SIDE_U32);
            encoder.set_color(png::ColorType::Rgba);
            let mut writer = encoder.write_header().expect("header");
            writer
                .write_chunk(png::chunk::ChunkType(kind), b"metadata")
                .expect("extra");
            writer
                .write_chunk(png::chunk::IDAT, &compressed(&raw))
                .expect("pixels");
        }
        assert_eq!(
            refusal(&bytes),
            "a chunk the avatar format does not allow",
            "{}",
            String::from_utf8_lossy(&kind)
        );
    }
}

#[test]
fn an_idat_that_inflates_beyond_the_image_is_refused_without_inflating_it() {
    // Zeros deflate a thousandfold: this much inflates to tens of megabytes. The cap on the
    // inflated size is the image's, so the stream is stopped one byte past it.
    let bomb = {
        let mut zip = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
        let block = vec![0u8; 1 << 20];
        for _ in 0..28 {
            zip.write_all(&block).expect("compress");
        }
        zip.finish().expect("zlib")
    };
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, SIDE_U32, SIDE_U32);
        encoder.set_color(png::ColorType::Rgba);
        let mut writer = encoder.write_header().expect("header");
        writer.write_chunk(png::chunk::IDAT, &bomb).expect("bomb");
    }
    assert!(
        bytes.len() <= MAX_AVATAR_BYTES,
        "the bomb is {} bytes",
        bytes.len()
    );
    assert_eq!(
        refusal(&bytes),
        "the pixel data does not inflate to exactly the image"
    );
}

#[test]
fn pixel_data_that_is_short_or_has_a_bad_filter_is_refused() {
    let raw = vec![0; SIDE * (1 + SIDE * 4)];
    let image = |raw: &[u8]| {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, SIDE_U32, SIDE_U32);
            encoder.set_color(png::ColorType::Rgba);
            let mut writer = encoder.write_header().expect("header");
            writer
                .write_chunk(png::chunk::IDAT, &compressed(raw))
                .expect("pixels");
        }
        bytes
    };
    assert_eq!(validate_avatar(&image(&raw)), Ok(()));
    assert_eq!(
        refusal(&image(&raw[..raw.len() - 1])),
        "the pixel data does not inflate to exactly the image"
    );
    let mut bad_filter = raw;
    bad_filter[0] = 5;
    assert_eq!(
        refusal(&image(&bad_filter)),
        "the PNG decoder refused the pixels"
    );
}

#[test]
fn eight_bit_non_interlaced_samples_are_the_only_ones_accepted() {
    for (depth, interlaced, expected) in [
        (png::BitDepth::Sixteen, false, "samples are not 8 bits"),
        (png::BitDepth::Four, false, "samples are not 8 bits"),
    ] {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, SIDE_U32, SIDE_U32);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(depth);
            let mut writer = encoder.write_header().expect("header");
            let _ = interlaced;
            writer
                .write_chunk(
                    png::chunk::IDAT,
                    &compressed(&vec![0; SIDE * (1 + SIDE * 2)]),
                )
                .expect("pixels");
        }
        assert_eq!(refusal(&bytes), expected, "{depth:?}");
    }
    let mut interlaced = flat(SIDE_U32, SIDE_U32);
    interlaced[8 + 8 + 12] = 1;
    assert_eq!(
        refusal(&interlaced),
        "an unknown compression or filter, or interlacing"
    );
}

#[test]
fn palette_and_transparency_chunks_that_make_no_sense_are_refused() {
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
            validate_avatar(&image_with_chunks(*color, chunks))
                .is_ok()
                .then_some(index)
        })
        .collect();
    assert!(
        accepted.is_empty(),
        "invalid chunk cases accepted: {accepted:?}"
    );
}

#[test]
fn every_supported_colour_type_with_its_transparency_is_accepted() {
    use png::ColorType::*;
    use png::chunk::{PLTE, tRNS};
    let cases: &[ChunkCase<'_>] = &[
        (Grayscale, &[]),
        (Grayscale, &[(tRNS, &[0, 128])]),
        (GrayscaleAlpha, &[]),
        (Rgb, &[]),
        (Rgb, &[(tRNS, &[0, 1, 0, 2, 0, 3])]),
        (Rgb, &[(PLTE, &[1, 2, 3]), (tRNS, &[0, 1, 0, 2, 0, 3])]),
        (Rgba, &[]),
        (Rgba, &[(PLTE, &[1, 2, 3])]),
        (Indexed, &[(PLTE, &[1, 2, 3]), (tRNS, &[128])]),
    ];
    for (color, chunks) in cases {
        assert_eq!(
            validate_avatar(&image_with_chunks(*color, chunks)),
            Ok(()),
            "{color:?}"
        );
    }
}

#[test]
fn a_transparency_chunk_with_a_bad_checksum_is_refused() {
    let mut bytes = image_with_chunks(
        png::ColorType::Indexed,
        &[(png::chunk::PLTE, &[0, 0, 0]), (png::chunk::tRNS, &[128])],
    );
    assert_eq!(validate_avatar(&bytes), Ok(()));
    let mut offset = 8;
    while offset < bytes.len() {
        let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().expect("chunk length"))
            as usize;
        if &bytes[offset + 4..offset + 8] == b"tRNS" {
            bytes[offset + 8 + length] ^= 1;
            break;
        }
        offset += 12 + length;
    }
    assert!(decode_avatar(&bytes).is_err());
}

#[test]
fn palette_indices_without_a_palette_entry_are_refused() {
    let bytes = encoded(
        png::ColorType::Indexed,
        &vec![1; SIDE * SIDE],
        Some((vec![255, 0, 0], vec![])),
    );
    assert_eq!(refusal(&bytes), "a pixel names a missing palette entry");
}

#[test]
fn canonical_bytes_drop_unused_palette_entries_and_their_transparency() {
    let mut palette = vec![3, 7, 9];
    palette.extend_from_slice(b"<script>payload</script>");
    let original = image_with_chunks(
        png::ColorType::Indexed,
        &[
            (png::chunk::PLTE, &palette),
            (png::chunk::tRNS, b"abcdefghi"),
        ],
    );
    let canonical = canonical_avatar(&original).expect("indexed image");
    assert!(
        !canonical.windows(8).any(|bytes| bytes == b"<script>"),
        "unused palette is not pixel data"
    );
    let decoded = decode_avatar(&canonical).expect("canonical palette");
    assert_eq!(decoded.palette, Some(vec![3, 7, 9]));
    assert_eq!(decoded.transparency, Some(vec![b'a']));
    assert!(decoded.samples.iter().all(|index| *index == 0));
    assert_eq!(canonical_avatar(&canonical).expect("stable"), canonical);
}

#[test]
fn palette_compaction_keeps_each_pixels_colour_and_alpha() {
    let pixels: Vec<u8> = (0..SIDE * SIDE)
        .map(|i| if i % 2 == 0 { 2 } else { 0 })
        .collect();
    let original = encoded(
        png::ColorType::Indexed,
        &pixels,
        Some((vec![255, 0, 0, 0, 255, 0, 0, 0, 255], vec![1, 2, 3])),
    );
    let canonical = canonical_avatar(&original).expect("indexed pixels");
    let decoded = decode_avatar(&canonical).expect("canonical");
    assert_eq!(decoded.palette, Some(vec![0, 0, 255, 255, 0, 0]));
    assert_eq!(decoded.transparency, Some(vec![3, 1]));
    for (index, pixel) in decoded.samples.iter().enumerate() {
        assert_eq!(*pixel, (index % 2) as u8);
    }
    assert_eq!(canonical_avatar(&canonical).expect("stable"), canonical);
}

#[test]
fn gray_and_rgb_pixel_transparency_survive_canonical_encoding() {
    for (color, transparency) in [
        (png::ColorType::Grayscale, vec![0, 128]),
        (png::ColorType::Rgb, vec![0, 128, 0, 64, 0, 32]),
    ] {
        let original = image_with_chunks(color, &[(png::chunk::tRNS, &transparency)]);
        let canonical = canonical_avatar(&original).expect("supported transparency");
        assert_eq!(canonical_avatar(&canonical).expect("stable"), canonical);
        let before = decode_avatar(&original).expect("pixels");
        let after = decode_avatar(&canonical).expect("canonical pixels");
        assert_eq!(after.transparency, before.transparency);
    }
}

#[test]
fn canonical_encoding_keeps_every_pixel_and_is_idempotent() {
    let pixels: Vec<u8> = (0..SIDE * SIDE)
        .flat_map(|index| {
            let (row, column) = (index / SIDE, index % SIDE);
            [
                (column / 4) as u8,
                (row / 4) as u8,
                ((row / 16) * (column / 16) % 251) as u8,
            ]
        })
        .collect();
    let original = encoded(png::ColorType::Rgb, &pixels, None);
    let canonical = canonical_avatar(&original).expect("canonical");
    assert_eq!(canonical_avatar(&canonical).expect("idempotent"), canonical);
    assert_eq!(decode_avatar(&canonical).expect("decoded").samples, pixels);
}

#[test]
fn rgba_expansion_applies_palette_and_colour_key_transparency() {
    let key = decode_avatar(&image_with_chunks(
        png::ColorType::Rgb,
        &[(png::chunk::tRNS, &[0, 0, 0, 0, 0, 0])],
    ))
    .expect("colour key");
    assert!(
        key.to_rgba()
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| *p == [0, 0, 0, 0])
    );
    let indexed = decode_avatar(&image_with_chunks(
        png::ColorType::Indexed,
        &[(png::chunk::PLTE, &[9, 8, 7]), (png::chunk::tRNS, &[40])],
    ))
    .expect("indexed");
    assert!(
        indexed
            .to_rgba()
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| *p == [9, 8, 7, 40])
    );
}

fn picture(mut color: impl FnMut(usize, usize) -> [u8; 4]) -> Vec<u8> {
    (0..SIDE * SIDE)
        .flat_map(|i| color(i / SIDE, i % SIDE))
        .collect()
}

/// Deterministic pseudo-random noise, so that tests do not depend on a seed.
fn noise(seed: u64) -> impl FnMut() -> u8 {
    let mut state = seed;
    move || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (state >> 56) as u8
    }
}

#[test]
fn a_flat_picture_is_kept_exactly_and_costs_few_bytes() {
    let rgba = picture(|row, column| {
        if (row / 32 + column / 32) % 2 == 0 {
            [200, 30, 30, 255]
        } else {
            [20, 40, 220, 255]
        }
    });
    let png = encode_avatar_rgba(&rgba).expect("fits");
    assert!(png.len() < 1024, "{} bytes", png.len());
    assert_eq!(decode_avatar(&png).expect("valid").to_rgba(), rgba);
    assert_eq!(canonical_avatar(&png).expect("canonical"), png);
}

#[test]
fn a_transparent_picture_keeps_its_alpha_and_hides_the_colour_of_invisible_pixels() {
    let mut random = noise(7);
    let rgba = picture(|row, column| {
        let junk = [random(), random(), random()];
        if (row as isize - 128).pow(2) + (column as isize - 128).pow(2) > 90 * 90 {
            [junk[0], junk[1], junk[2], 0]
        } else {
            [200, 100, 50, 255 - (row as u8 / 2)]
        }
    });
    let png = encode_avatar_rgba(&rgba).expect("fits");
    let decoded = decode_avatar(&png).expect("valid").to_rgba();
    for (before, after) in rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(decoded.as_chunks::<4>().0)
    {
        if before[3] == 0 {
            assert_eq!(after, &[0, 0, 0, 0]);
        } else {
            assert_eq!(before, after);
        }
    }
}

#[test]
fn a_photograph_like_picture_loses_colours_only_as_far_as_the_limit_needs() {
    let mut random = noise(11);
    let rgba = picture(|row, column| {
        let wave = (row as f32 / 17.0).sin() * 40.0 + (column as f32 / 23.0).cos() * 40.0;
        let mut grain = |scale: u8| (random() / scale) as f32;
        [
            (120.0 + wave + grain(8)).clamp(0.0, 255.0) as u8,
            (90.0 - wave + grain(8)).clamp(0.0, 255.0) as u8,
            (160.0 + wave / 2.0 + grain(8)).clamp(0.0, 255.0) as u8,
            255,
        ]
    });
    let png = encode_avatar_rgba(&rgba).expect("fits after reducing colours");
    assert!(png.len() <= compress::FIT_BYTES, "{} bytes", png.len());
    assert!(
        png.len() > compress::FIT_BYTES / 4,
        "reduced further than needed: {} bytes",
        png.len()
    );
    let decoded = decode_avatar(&png).expect("valid");
    assert_eq!(decoded.layout, Layout::Indexed);
    let worst = rgba
        .iter()
        .zip(decoded.to_rgba())
        .map(|(before, after)| before.abs_diff(after))
        .max()
        .unwrap_or(0);
    assert!(worst < 96, "a pixel is {worst} off");
    assert_eq!(canonical_avatar(&png).expect("canonical"), png);
}

#[test]
fn full_noise_still_fits_by_dropping_to_few_colours() {
    let mut random = noise(3);
    let rgba = picture(|_, _| [random(), random(), random(), 255]);
    let png = encode_avatar_rgba(&rgba).expect("fits");
    assert!(png.len() <= compress::FIT_BYTES);
    assert_eq!(validate_avatar(&png), Ok(()));
}

#[test]
fn a_picture_of_the_wrong_length_is_refused_with_its_reason() {
    assert_eq!(
        encode_avatar_rgba(&[0; 16]),
        Err(AvatarError("the picture is not exactly the avatar size"))
    );
}

#[test]
fn a_gray_picture_is_stored_as_gray() {
    let rgba = picture(|row, column| {
        let v = ((row + column) / 2) as u8;
        [v, v, v, 255]
    });
    let png = encode_avatar_rgba(&rgba).expect("fits");
    let decoded = decode_avatar(&png).expect("valid");
    assert!(matches!(decoded.layout, Layout::Gray | Layout::Indexed));
    assert_eq!(decoded.to_rgba(), rgba);
}
