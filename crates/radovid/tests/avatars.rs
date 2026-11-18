//! Avatars through a running service: what one account sets, another sees; and what the format
//! forbids is refused, each with the same exact refusal.
mod common;
use common::*;
use radovid_client::proto::*;
use std::io::Write;

const SIDE: usize = AVATAR_SIDE as usize;

fn flat_picture() -> Vec<u8> {
    (0..SIDE * SIDE)
        .flat_map(|index| {
            if (index / SIDE / 64 + index % SIDE / 64) % 2 == 0 {
                [220, 40, 40, 255]
            } else {
                [30, 60, 200, 255]
            }
        })
        .collect()
}

fn png_with(width: u32, height: u32, chunk: Option<&[u8; 4]>, scanlines: &[u8]) -> Vec<u8> {
    let mut zip = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    zip.write_all(scanlines).expect("compress");
    let pixels = zip.finish().expect("zlib");
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("header");
        if let Some(kind) = chunk {
            writer
                .write_chunk(png::chunk::ChunkType(*kind), b"payload")
                .expect("extra chunk");
        }
        writer
            .write_chunk(png::chunk::IDAT, &pixels)
            .expect("pixels");
    }
    bytes
}

fn set(account: &mut Account, png: Vec<u8>) -> Result<Reply, Error> {
    account.ask(ClientMsg::SetAvatar { png })
}

#[test]
fn an_avatar_one_account_sets_is_served_to_another_and_to_its_profile_and_cached_versions() {
    let env = start(vec![ruleset("solo", true, "team0", "0.02")]);
    let mut alpha = env.human("alpha");
    let mut beta = env.human("beta");
    let png = encode_avatar_rgba(&flat_picture()).expect("a flat picture");
    let Ok(Reply::Avatar {
        account,
        avatar: Some(stored),
    }) = set(&mut alpha, png.clone())
    else {
        panic!("the upload is stored");
    };
    assert_eq!(account, alpha.me.id);
    assert_eq!(stored.png, png, "the encoder's bytes are the stored bytes");
    assert_eq!(
        beta.ask(ClientMsg::GetAvatar {
            account: alpha.me.id
        }),
        Ok(Reply::Avatar {
            account: alpha.me.id,
            avatar: Some(stored.clone())
        })
    );
    let Ok(Reply::Account { account: profile }) = beta.ask(ClientMsg::Account {
        account: alpha.me.id,
    }) else {
        panic!("a public profile");
    };
    assert_eq!(profile.avatar, Some(stored.version));
    let asked = |version| ClientMsg::GetAvatars {
        accounts: vec![AvatarQuery {
            account: alpha.me.id,
            version,
        }],
    };
    assert_eq!(
        beta.ask(asked(None)),
        Ok(Reply::Avatars {
            changes: vec![AvatarChange {
                account: alpha.me.id,
                avatar: Some(stored.clone())
            }],
            answered: 1
        })
    );
    assert_eq!(
        beta.ask(asked(Some(stored.version))),
        Ok(Reply::Avatars {
            changes: Vec::new(),
            answered: 1
        })
    );
}

#[test]
fn every_way_to_break_the_format_is_refused_with_bad_avatar_and_stores_nothing() {
    let env = start(vec![ruleset("solo", true, "team0", "0.02")]);
    let side = SIDE as u32;
    let scanlines = vec![0u8; SIDE * (1 + SIDE * 4)];
    let mut bomb = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    for _ in 0..28 {
        bomb.write_all(&vec![0u8; 1 << 20]).expect("compress");
    }
    let bomb = bomb.finish().expect("zlib");
    let mut bomb_png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bomb_png, side, side);
        encoder.set_color(png::ColorType::Rgba);
        let mut writer = encoder.write_header().expect("header");
        writer
            .write_chunk(png::chunk::IDAT, &bomb)
            .expect("bomb pixels");
    }
    let mut trailing = png_with(side, side, None, &scanlines);
    trailing.push(0);
    let mut truncated = png_with(side, side, None, &scanlines);
    truncated.truncate(truncated.len() - 5);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        (
            "the old 32x32 size",
            png_with(32, 32, None, &vec![0u8; 32 * (1 + 32 * 4)]),
        ),
        (
            "one row short",
            png_with(
                side,
                side - 1,
                None,
                &vec![0u8; (SIDE - 1) * (1 + SIDE * 4)],
            ),
        ),
        (
            "a wider picture",
            png_with(side * 2, side, None, &vec![0u8; SIDE * (1 + SIDE * 8)]),
        ),
        (
            "a text chunk",
            png_with(side, side, Some(b"tEXt"), &scanlines),
        ),
        (
            "an animation chunk",
            png_with(side, side, Some(b"acTL"), &scanlines),
        ),
        (
            "a colour profile",
            png_with(side, side, Some(b"iCCP"), &scanlines),
        ),
        ("a decompression bomb", bomb_png),
        ("trailing bytes", trailing),
        ("a truncated file", truncated),
        ("not an image", b"<svg onload='alert(1)'/>".to_vec()),
    ];
    for (index, (what, png)) in cases.into_iter().enumerate() {
        assert!(
            png.len() <= MAX_AVATAR_BYTES,
            "{what} must reach the format check"
        );
        let mut account = env.human(&format!("hostile{index}"));
        assert_eq!(set(&mut account, png), Err(Error::BadAvatar), "{what}");
        assert_eq!(
            account.ask(ClientMsg::GetAvatar {
                account: account.me.id
            }),
            Ok(Reply::Avatar {
                account: account.me.id,
                avatar: None
            }),
            "{what} stored nothing"
        );
    }
}
