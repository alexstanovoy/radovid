//! Makes an avatar of any 256 by 256 picture: the picture itself when it fits the byte limit,
//! else the picture with fewer colours, as many as the limit allows.
use super::{AvatarError, AvatarImage, Layout, SIDE, TOO_LARGE};
use crate::MAX_AVATAR_BYTES;

/// What a client aims at, a little under [`MAX_AVATAR_BYTES`]: the service encodes the image
/// again before it stores it, and another build of the PNG encoder may spend a few more bytes
/// than the client's did.
pub const FIT_BYTES: usize = MAX_AVATAR_BYTES - MAX_AVATAR_BYTES / 16;

/// Palette sizes tried when the picture does not fit as it is, largest first.
const PALETTE_STEPS: [usize; 12] = [256, 192, 128, 96, 64, 48, 32, 24, 16, 8, 4, 2];

/// Encodes `rgba`, [`AVATAR_SIDE`](crate::AVATAR_SIDE) squared 8-bit RGBA pixels row by row, as
/// the canonical avatar PNG of at most [`FIT_BYTES`]. Colours are reduced only when the picture
/// does not fit as it is, and then by the least that fits.
pub fn encode_avatar_rgba(rgba: &[u8]) -> Result<Vec<u8>, AvatarError> {
    if rgba.len() != SIDE * SIDE * 4 {
        return Err(AvatarError("the picture is not exactly the avatar size"));
    }
    let pixels = hide_invisible_colors(rgba);
    let colors = Colors::of(&pixels);
    let mut best: Option<Vec<u8>> = None;
    for image in lossless_candidates(&pixels, &colors) {
        match image.canonical_png() {
            Ok(png) if png.len() <= FIT_BYTES => {
                if best.as_ref().is_none_or(|best| png.len() < best.len()) {
                    best = Some(png);
                }
            }
            Ok(_) | Err(TOO_LARGE) => {}
            Err(error) => return Err(error),
        }
    }
    if let Some(png) = best {
        return Ok(png);
    }
    reduced(&pixels, &colors)
}

/// The colour of a fully transparent pixel is not seen; one colour for all of them compresses
/// better and keeps the picture's invisible parts from carrying anything.
fn hide_invisible_colors(rgba: &[u8]) -> Vec<[u8; 4]> {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| if pixel[3] == 0 { [0; 4] } else { *pixel })
        .collect()
}

/// The distinct colours of a picture and how many pixels have each, ordered by colour.
struct Colors {
    entries: Vec<([u8; 4], u32)>,
}

impl Colors {
    fn of(pixels: &[[u8; 4]]) -> Colors {
        let mut sorted: Vec<u32> = pixels
            .iter()
            .map(|pixel| u32::from_be_bytes(*pixel))
            .collect();
        sorted.sort_unstable();
        let mut entries: Vec<([u8; 4], u32)> = Vec::new();
        for color in sorted {
            match entries.last_mut() {
                Some((last, count)) if u32::from_be_bytes(*last) == color => *count += 1,
                _ => entries.push((color.to_be_bytes(), 1)),
            }
        }
        Colors { entries }
    }

    fn opaque(&self) -> bool {
        self.entries.iter().all(|(color, _)| color[3] == 255)
    }

    fn gray(&self) -> bool {
        self.entries
            .iter()
            .all(|(color, _)| color[0] == color[1] && color[1] == color[2])
    }
}

/// The picture exactly, in each of the layouts that can hold it.
fn lossless_candidates(pixels: &[[u8; 4]], colors: &Colors) -> Vec<AvatarImage> {
    let opaque = colors.opaque();
    let mut candidates = Vec::new();
    if colors.entries.len() <= 256 {
        candidates.push(indexed(pixels, palette_of(colors)));
    }
    if colors.gray() {
        let samples = if opaque {
            pixels.iter().map(|pixel| pixel[0]).collect()
        } else {
            pixels
                .iter()
                .flat_map(|pixel| [pixel[0], pixel[3]])
                .collect()
        };
        let layout = if opaque {
            Layout::Gray
        } else {
            Layout::GrayAlpha
        };
        candidates.push(plain(layout, samples));
    }
    if opaque {
        let samples = pixels
            .iter()
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect();
        candidates.push(plain(Layout::Rgb, samples));
    } else {
        candidates.push(plain(Layout::Rgba, pixels.as_flattened().to_vec()));
    }
    candidates
}

fn plain(layout: Layout, samples: Vec<u8>) -> AvatarImage {
    AvatarImage {
        layout,
        samples,
        palette: None,
        transparency: None,
    }
}

fn palette_of(colors: &Colors) -> Vec<[u8; 4]> {
    colors.entries.iter().map(|(color, _)| *color).collect()
}

/// An indexed image whose every pixel is exactly a palette colour.
fn indexed(pixels: &[[u8; 4]], palette: Vec<[u8; 4]>) -> AvatarImage {
    let mut lookup = std::collections::HashMap::with_capacity(palette.len());
    for (index, color) in palette.iter().enumerate() {
        lookup.insert(*color, index as u8);
    }
    let samples = pixels.iter().map(|pixel| lookup[pixel]).collect();
    with_palette(samples, &palette)
}

fn with_palette(samples: Vec<u8>, palette: &[[u8; 4]]) -> AvatarImage {
    let transparency: Vec<u8> = palette.iter().map(|color| color[3]).collect();
    AvatarImage {
        layout: Layout::Indexed,
        samples,
        palette: Some(
            palette
                .iter()
                .flat_map(|color| [color[0], color[1], color[2]])
                .collect(),
        ),
        transparency: transparency
            .iter()
            .any(|alpha| *alpha != 255)
            .then_some(transparency),
    }
}

/// The picture with the most colours that still fits, found by halving the search over
/// [`PALETTE_STEPS`] (sizes shrink almost monotonically with fewer colours).
fn reduced(pixels: &[[u8; 4]], colors: &Colors) -> Result<Vec<u8>, AvatarError> {
    let try_size = |size: usize| -> Result<Option<Vec<u8>>, AvatarError> {
        let palette = median_cut(colors, size);
        match dithered(pixels, &palette).canonical_png() {
            Ok(png) => Ok((png.len() <= FIT_BYTES).then_some(png)),
            Err(TOO_LARGE) => Ok(None),
            Err(error) => Err(error),
        }
    };
    let (mut low, mut high) = (0, PALETTE_STEPS.len());
    let mut best = None;
    // `high` is the first step known not to be needed; steps from `high` on are smaller.
    while low < high {
        let middle = (low + high) / 2;
        match try_size(PALETTE_STEPS[middle])? {
            Some(png) => {
                best = Some(png);
                high = middle;
            }
            None => low = middle + 1,
        }
    }
    best.ok_or(AvatarError(
        "no number of colours makes the picture fit the byte limit",
    ))
}

#[derive(Clone, Copy)]
struct Weighted {
    color: [u8; 4],
    count: u32,
}

/// Median cut: the box with the most to lose is split at the pixel-weighted middle of its widest
/// channel until there are `size` boxes; a box's colour is its pixels' mean.
fn median_cut(colors: &Colors, size: usize) -> Vec<[u8; 4]> {
    let all: Vec<Weighted> = colors
        .entries
        .iter()
        .map(|(color, count)| Weighted {
            color: *color,
            count: *count,
        })
        .collect();
    if all.len() <= size {
        return all.iter().map(|entry| entry.color).collect();
    }
    let mut boxes = vec![all];
    while boxes.len() < size {
        let Some((at, channel)) = boxes
            .iter()
            .enumerate()
            .filter(|(_, entries)| entries.len() > 1)
            .map(|(at, entries)| {
                let (channel, range) = widest(entries);
                let weight: u64 = entries.iter().map(|entry| u64::from(entry.count)).sum();
                (at, channel, weight * u64::from(range) * u64::from(range))
            })
            .max_by_key(|(_, _, loss)| *loss)
            .map(|(at, channel, _)| (at, channel))
        else {
            break;
        };
        let mut entries = boxes.swap_remove(at);
        entries.sort_unstable_by_key(|entry| entry.color[channel]);
        let half = entries
            .iter()
            .map(|entry| u64::from(entry.count))
            .sum::<u64>()
            / 2;
        let mut seen = 0;
        let mut split = entries.len() - 1;
        for (index, entry) in entries.iter().enumerate() {
            seen += u64::from(entry.count);
            if seen >= half {
                split = (index + 1).min(entries.len() - 1);
                break;
            }
        }
        let upper = entries.split_off(split);
        boxes.push(entries);
        boxes.push(upper);
    }
    boxes.iter().map(|entries| mean(entries)).collect()
}

/// The channel whose spread (weighted so that the eye's sensitivity counts) is widest.
fn widest(entries: &[Weighted]) -> (usize, u32) {
    const EYE: [u32; 4] = [3, 5, 2, 4];
    let mut best = (0, 0);
    for (channel, weight) in EYE.iter().enumerate() {
        let low = entries
            .iter()
            .map(|entry| entry.color[channel])
            .min()
            .unwrap_or(0);
        let high = entries
            .iter()
            .map(|entry| entry.color[channel])
            .max()
            .unwrap_or(0);
        let range = u32::from(high - low) * weight;
        if range > best.1 {
            best = (channel, range);
        }
    }
    best
}

fn mean(entries: &[Weighted]) -> [u8; 4] {
    let total: u64 = entries.iter().map(|entry| u64::from(entry.count)).sum();
    let mut color = [0u8; 4];
    for (channel, out) in color.iter_mut().enumerate() {
        let sum: u64 = entries
            .iter()
            .map(|entry| u64::from(entry.color[channel]) * u64::from(entry.count))
            .sum();
        *out = ((sum + total / 2) / total) as u8;
    }
    if color[3] == 0 {
        return [0; 4];
    }
    color
}

/// Floyd-Steinberg dithering of the picture onto `palette`, left to right and back again by row.
fn dithered(pixels: &[[u8; 4]], palette: &[[u8; 4]]) -> AvatarImage {
    let mut error = vec![[0i32; 4]; (SIDE + 2) * 2];
    let (mut this, mut next) = (0, SIDE + 2);
    let mut samples = vec![0u8; SIDE * SIDE];
    for row in 0..SIDE {
        let forward = row % 2 == 0;
        for step in 0..SIDE {
            let column = if forward { step } else { SIDE - 1 - step };
            let pixel = pixels[row * SIDE + column];
            let wanted: [i32; 4] = std::array::from_fn(|channel| {
                (i32::from(pixel[channel]) + error[this + column + 1][channel] / 16).clamp(0, 255)
            });
            let index = nearest(palette, wanted);
            samples[row * SIDE + column] = index as u8;
            let chosen = palette[index];
            let miss: [i32; 4] =
                std::array::from_fn(|channel| wanted[channel] - i32::from(chosen[channel]));
            // A fully transparent pixel has no colour to be wrong about.
            let miss = if pixel[3] == 0 { [0; 4] } else { miss };

            let (ahead, behind): (isize, isize) = if forward { (1, -1) } else { (-1, 1) };
            let spread = [
                (this, ahead, 7),
                (next, behind, 3),
                (next, 0, 5),
                (next, ahead, 1),
            ];
            for (line, shift, share) in spread {
                let at = (line as isize + column as isize + 1 + shift) as usize;
                for channel in 0..4 {
                    error[at][channel] += miss[channel] * share;
                }
            }
        }
        error[this..this + SIDE + 2].fill([0; 4]);
        std::mem::swap(&mut this, &mut next);
    }
    with_palette(samples, palette)
}

/// The palette entry closest to `wanted`, by squared distance with the eye's weights.
fn nearest(palette: &[[u8; 4]], wanted: [i32; 4]) -> usize {
    const EYE: [i32; 4] = [3, 5, 2, 4];
    let mut best = (0, i64::MAX);
    for (index, color) in palette.iter().enumerate() {
        let mut distance = 0i64;
        for channel in 0..4 {
            let delta = i64::from(wanted[channel] - i32::from(color[channel]));
            distance += delta * delta * i64::from(EYE[channel]);
        }
        if distance < best.1 {
            best = (index, distance);
        }
    }
    best.0
}
