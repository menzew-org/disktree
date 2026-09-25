//! `cargo xtask icon`: the Windows icon, rendered from the SVG mark.
//!
//! The mark is nothing but axis-aligned rectangles on a 64-unit grid, so it
//! is rasterized here directly — supersampled for clean edges at small
//! sizes — and packed into an `.ico`, with no image library and no tool to
//! install. Rerun it whenever `assets/disktree.svg` changes; the build
//! embeds the result in `disktree.exe`.

use std::fmt::Write as _;
use std::path::Path;

const SOURCE: &str = "assets/disktree.svg";
const TARGET: &str = "assets/disktree.ico";
/// What Windows asks for, from the taskbar's small icon to Explorer's
/// largest view.
const SIZES: [u32; 7] = [16, 20, 24, 32, 48, 64, 256];
/// Samples per pixel along each axis.
const SUPERSAMPLE: u32 = 8;

/// One `<rect>`: position and size in the 64-unit grid, and its colour.
struct Rect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    rgb: [u8; 3],
}

pub fn render() -> Result<(), String> {
    let svg = std::fs::read_to_string(SOURCE)
        .map_err(|error| format!("{SOURCE}: {error}"))?;
    let rects = parse(&svg)?;
    let images: Vec<(u32, Vec<u8>)> = SIZES
        .iter()
        .map(|&size| (size, bitmap(&rects, size)))
        .collect();
    std::fs::write(Path::new(TARGET), ico(&images))
        .map_err(|error| format!("{TARGET}: {error}"))?;
    let mut sizes = String::new();
    for size in SIZES {
        let _ = write!(sizes, " {size}");
    }
    println!("wrote {TARGET}:{sizes} px");
    Ok(())
}

/// Every `<rect>` in the SVG, in paint order. Anything else is refused
/// rather than silently dropped, so a change to the mark cannot go
/// unnoticed here.
fn parse(svg: &str) -> Result<Vec<Rect>, String> {
    let mut rects = Vec::new();
    for element in svg.split('<').skip(1) {
        let tag = element.split_whitespace().next().unwrap_or_default();
        match tag.trim_end_matches('>') {
            "rect" | "rect/" => rects.push(rect(element)?),
            "svg" | "/svg" | "?xml" | "!--" => {}
            other => {
                return Err(format!(
                    "{SOURCE}: <{other}> is not a rectangle; extend \
                     xtask/src/icon.rs to draw it"
                ));
            }
        }
    }
    Ok(rects)
}

fn rect(element: &str) -> Result<Rect, String> {
    let attribute = |name: &str| -> Option<&str> {
        let start = element.find(&format!(" {name}=\""))? + name.len() + 3;
        let end = element[start..].find('"')? + start;
        Some(&element[start..end])
    };
    let number = |name: &str| -> Result<f64, String> {
        attribute(name).map_or(Ok(0.0), |value| {
            value
                .parse()
                .map_err(|_| format!("{SOURCE}: bad {name}=\"{value}\""))
        })
    };
    let fill = attribute("fill").unwrap_or("#000000");
    let hex = fill
        .strip_prefix('#')
        .filter(|hex| hex.len() == 6)
        .ok_or_else(|| format!("{SOURCE}: fill {fill} is not #rrggbb"))?;
    let channel = |at: usize| {
        u8::from_str_radix(&hex[at..at + 2], 16)
            .map_err(|_| format!("{SOURCE}: bad colour {fill}"))
    };
    Ok(Rect {
        x: number("x")?,
        y: number("y")?,
        width: number("width")?,
        height: number("height")?,
        rgb: [channel(0)?, channel(2)?, channel(4)?],
    })
}

/// A `size`×`size` BGRA image, bottom row first as a DIB stores it.
fn bitmap(rects: &[Rect], size: u32) -> Vec<u8> {
    let samples = f64::from(size * SUPERSAMPLE);
    let scale = samples / 64.0;
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    for row in (0..size).rev() {
        for column in 0..size {
            let mut sum = [0.0_f64; 4];
            for sy in 0..SUPERSAMPLE {
                for sx in 0..SUPERSAMPLE {
                    let x = f64::from(column * SUPERSAMPLE + sx) + 0.5;
                    let y = f64::from(row * SUPERSAMPLE + sy) + 0.5;
                    // The last rectangle under the sample wins, as it
                    // would when painted in order.
                    if let Some(rect) = rects.iter().rev().find(|rect| {
                        x >= rect.x * scale
                            && x < (rect.x + rect.width) * scale
                            && y >= rect.y * scale
                            && y < (rect.y + rect.height) * scale
                    }) {
                        for (total, value) in sum.iter_mut().zip(rect.rgb) {
                            *total += f64::from(value);
                        }
                        sum[3] += 255.0;
                    }
                }
            }
            let count = f64::from(SUPERSAMPLE * SUPERSAMPLE);
            let [r, g, b, a] = sum.map(|total| (total / count).round() as u8);
            pixels.extend_from_slice(&[b, g, r, a]);
        }
    }
    pixels
}

/// Pack images into an `.ico`: a directory, then each image as a 32-bit
/// DIB with its (all-opaque, since alpha rules) AND mask.
fn ico(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let count = u16::try_from(images.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&count.to_le_bytes());
    // The 256 px image is stored as PNG, as Windows expects since Vista;
    // the small ones as plain DIBs, which every loader reads.
    let entries: Vec<Vec<u8>> = images
        .iter()
        .map(|(size, pixels)| {
            if *size >= 256 {
                png(*size, pixels)
            } else {
                dib(*size, pixels)
            }
        })
        .collect();
    let mut offset = 6 + 16 * u32::from(count);
    for ((size, _), data) in images.iter().zip(&entries) {
        // A width or height of 256 is written as 0.
        let side = u8::try_from(*size).unwrap_or(0);
        out.extend_from_slice(&[side, side, 0, 0]);
        out.extend_from_slice(&1_u16.to_le_bytes());
        out.extend_from_slice(&32_u16.to_le_bytes());
        let length = u32::try_from(data.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&length.to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += length;
    }
    for data in entries {
        out.extend_from_slice(&data);
    }
    out
}

/// A PNG of a bottom-up BGRA image: RGBA rows top first, deflated with
/// stored blocks — larger than compressed, but with no library to add.
fn png(size: u32, pixels: &[u8]) -> Vec<u8> {
    let row = (size * 4) as usize;
    let mut raw = Vec::with_capacity((row + 1) * size as usize);
    for line in pixels.chunks_exact(row).rev() {
        raw.push(0); // no filter
        for pixel in line.chunks_exact(4) {
            raw.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
        }
    }
    let mut zlib = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (index, block) in blocks.iter().enumerate() {
        zlib.push(u8::from(index + 1 == blocks.len()));
        let length = u16::try_from(block.len()).unwrap_or(u16::MAX);
        zlib.extend_from_slice(&length.to_le_bytes());
        zlib.extend_from_slice(&(!length).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut header = Vec::new();
    header.extend_from_slice(&size.to_be_bytes());
    header.extend_from_slice(&size.to_be_bytes());
    // 8-bit RGBA, default compression, filtering and no interlace.
    header.extend_from_slice(&[8, 6, 0, 0, 0]);

    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    for (kind, data) in [
        (*b"IHDR", header.as_slice()),
        (*b"IDAT", zlib.as_slice()),
        (*b"IEND", &[][..]),
    ] {
        let length = u32::try_from(data.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&length.to_be_bytes());
        let start = out.len();
        out.extend_from_slice(&kind);
        out.extend_from_slice(data);
        let crc = crc32(&out[start..]);
        out.extend_from_slice(&crc.to_be_bytes());
    }
    out
}

fn crc32(bytes: &[u8]) -> u32 {
    !bytes.iter().fold(u32::MAX, |crc, byte| {
        (0..8).fold(crc ^ u32::from(*byte), |crc, _| {
            (crc >> 1) ^ (0xedb8_8320 & 0_u32.wrapping_sub(crc & 1))
        })
    })
}

fn adler32(bytes: &[u8]) -> u32 {
    let (a, b) = bytes.iter().fold((1_u32, 0_u32), |(a, b), byte| {
        let a = (a + u32::from(*byte)) % 65_521;
        (a, (b + a) % 65_521)
    });
    (b << 16) | a
}

fn dib(size: u32, pixels: &[u8]) -> Vec<u8> {
    // Each mask row is padded to four bytes.
    let mask_row = size.div_ceil(32) * 4;
    let mut out = Vec::new();
    out.extend_from_slice(&40_u32.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    // The height covers the image and its mask.
    out.extend_from_slice(&(size * 2).to_le_bytes());
    out.extend_from_slice(&1_u16.to_le_bytes());
    out.extend_from_slice(&32_u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    out.extend_from_slice(pixels);
    out.resize(out.len() + (mask_row * size) as usize, 0);
    out
}
