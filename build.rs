//! Compile-time GIF -> JTM1 default-movie blob.
//!
//! Reads `assets/movie.gif`, stretches every frame to 128x64, applies 8x8 Bayer
//! ordered dithering to 1-bit, and packs into the SSD1309 page layout. Each
//! frame is then encoded two ways - raw, and XOR-delta + RLE - and the smaller
//! total wins. The result is written as one JTM1 blob:
//!   * `$OUT_DIR/default_movie.bin` - the baked-in default movie
//!
//! The same JTM1 layout is produced by the browser uploader and parsed by one
//! decoder on the device (see PLAN.md §4). build.rs also runs
//! `embuild::espidf::sysenv::output()` so the ESP-IDF link step still works -
//! the GIF pipeline and the IDF wiring share this one build script.

use std::env;
use std::fs;
use std::io::BufReader;
use std::path::Path;

use image::codecs::gif::GifDecoder;
use image::imageops::FilterType;
use image::{AnimationDecoder, DynamicImage};

const WIDTH: u32 = 128;
const HEIGHT: u32 = 64;
const FRAME_BYTES: usize = (WIDTH as usize * HEIGHT as usize) / 8; // 1024
const MIN_DELAY_MS: u16 = 30;

/// 8x8 Bayer ordered-dither matrix (values 0..=63). Stable per pixel position,
/// so static regions of the movie do not shimmer between frames. The browser
/// uploader (web/index.html) carries an identical copy.
const BAYER8: [[u8; 8]; 8] = [
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

fn main() {
    convert_gif();
    // ESP-IDF linker / sysenv setup - required for the device build to link.
    embuild::espidf::sysenv::output();
}

fn convert_gif() {
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap();
    let gif_path = Path::new(&manifest).join("assets/movie.gif");
    println!("cargo:rerun-if-changed={}", gif_path.display());

    let file = fs::File::open(&gif_path)
        .unwrap_or_else(|e| panic!("cannot open {}: {e}", gif_path.display()));
    let decoder =
        GifDecoder::new(BufReader::new(file)).expect("failed to create GIF decoder");
    let frames = decoder
        .into_frames()
        .collect_frames()
        .expect("failed to decode GIF frames");
    assert!(!frames.is_empty(), "GIF contains no frames");
    assert!(
        frames.len() <= u16::MAX as usize,
        "GIF has too many frames for the JTM1 u16 frame count"
    );

    // Decode + dither + pack every frame into the SSD1309 page layout.
    let mut packed: Vec<Vec<u8>> = Vec::with_capacity(frames.len());
    let mut delays: Vec<u16> = Vec::with_capacity(frames.len());
    for frame in &frames {
        let (num, den) = frame.delay().numer_denom_ms();
        let ms = if den == 0 { 100 } else { num / den };
        delays.push((ms as u16).max(MIN_DELAY_MS));
        packed.push(pack_frame(frame));
    }

    // Encode raw and delta+RLE; keep whichever blob is smaller.
    let (format, blob, offsets) = choose_encoding(&packed);
    let jtm1 = build_jtm1(format, &delays, &offsets, &blob);

    let out_dir = env::var("OUT_DIR").unwrap();
    fs::write(Path::new(&out_dir).join("default_movie.bin"), &jtm1).unwrap();

    let raw_total = packed.len() * FRAME_BYTES;
    println!(
        "cargo:warning=default movie: {} frames, FORMAT={} ({}), JTM1 blob {} bytes \
         (frame data {}% of raw {})",
        frames.len(),
        format,
        if format == 0 { "raw" } else { "delta+RLE" },
        jtm1.len(),
        blob.len() * 100 / raw_total.max(1),
        raw_total,
    );
}

/// Stretch one GIF frame to 128x64, Bayer-dither to 1-bit, and pack it into the
/// SSD1309 page layout: each byte is 8 vertically stacked pixels, bit 0 = top.
fn pack_frame(frame: &image::Frame) -> Vec<u8> {
    let luma = DynamicImage::ImageRgba8(frame.buffer().clone())
        .resize_exact(WIDTH, HEIGHT, FilterType::Triangle)
        .to_luma8();
    let mut packed = vec![0u8; FRAME_BYTES];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let lum = luma.get_pixel(x, y).0[0] as u16;
            let threshold = BAYER8[(y % 8) as usize][(x % 8) as usize] as u16 * 4;
            if lum > threshold {
                let page = (y / 8) as usize;
                packed[page * WIDTH as usize + x as usize] |= 1u8 << (y % 8);
            }
        }
    }
    packed
}

/// Encode the packed frames raw and as XOR-delta + RLE; return the smaller.
/// Result is `(format, blob, offsets)` where frame `i` is `blob[offsets[i]..[i+1]]`.
fn choose_encoding(packed: &[Vec<u8>]) -> (u8, Vec<u8>, Vec<u32>) {
    // Raw: each frame is a verbatim 1024-byte page buffer.
    let mut raw = Vec::with_capacity(packed.len() * FRAME_BYTES);
    let mut raw_offsets = vec![0u32];
    for f in packed {
        raw.extend_from_slice(f);
        raw_offsets.push(raw.len() as u32);
    }

    // Delta+RLE: frame 0 is XORed against zeros (so it stands alone); every
    // later frame is XORed against its predecessor, then run-length encoded.
    let mut delta = Vec::new();
    let mut delta_offsets = vec![0u32];
    let mut prev = vec![0u8; FRAME_BYTES];
    for f in packed {
        let diff: Vec<u8> = f.iter().zip(&prev).map(|(a, b)| a ^ b).collect();
        delta.extend_from_slice(&rle_encode(&diff));
        delta_offsets.push(delta.len() as u32);
        prev.clone_from(f);
    }

    if delta.len() < raw.len() {
        (1, delta, delta_offsets)
    } else {
        (0, raw, raw_offsets)
    }
}

/// Run-length encode one XOR-delta buffer.
///
/// Op stream of 2-byte little-endian headers: bit 15 = type (0 = COPY, an
/// unchanged run; 1 = XOR, a literal run), bits 0..14 = run length. XOR ops are
/// followed by `len` literal delta bytes. Short zero gaps inside a changed
/// region are absorbed into the literal run rather than split into their own op.
fn rle_encode(diff: &[u8]) -> Vec<u8> {
    const ZERO_GAP: usize = 4; // shorter zero runs stay inside a literal run
    let n = diff.len();
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < n {
        if diff[pos] == 0 {
            let start = pos;
            while pos < n && diff[pos] == 0 {
                pos += 1;
            }
            emit_op(&mut out, false, pos - start, &[]);
        } else {
            let start = pos;
            while pos < n {
                if diff[pos] != 0 {
                    pos += 1;
                } else {
                    let mut z = pos;
                    while z < n && diff[z] == 0 {
                        z += 1;
                    }
                    if z - pos >= ZERO_GAP {
                        break;
                    }
                    pos = z; // absorb the short gap
                }
            }
            emit_op(&mut out, true, pos - start, &diff[start..pos]);
        }
    }
    out
}

fn emit_op(out: &mut Vec<u8>, is_xor: bool, len: usize, literals: &[u8]) {
    assert!((1..=0x7FFF).contains(&len), "RLE run out of range: {len}");
    let header = len as u16 | if is_xor { 0x8000 } else { 0 };
    out.extend_from_slice(&header.to_le_bytes());
    if is_xor {
        out.extend_from_slice(literals);
    }
}

/// Assemble a JTM1 blob (see PLAN.md §4): magic, format, reserved, frame count,
/// per-frame delays, per-frame offsets, then the frame data back to back.
fn build_jtm1(format: u8, delays: &[u16], offsets: &[u32], data: &[u8]) -> Vec<u8> {
    let n = delays.len();
    assert_eq!(offsets.len(), n + 1, "offsets must have frame_count + 1 entries");
    let mut out = Vec::with_capacity(8 + 2 * n + 4 * (n + 1) + data.len());
    out.extend_from_slice(b"JTM1"); // magic
    out.push(format); // 0 = raw, 1 = XOR-delta + RLE
    out.push(0); // reserved
    out.extend_from_slice(&(n as u16).to_le_bytes()); // frame_count
    for &d in delays {
        out.extend_from_slice(&d.to_le_bytes());
    }
    for &o in offsets {
        out.extend_from_slice(&o.to_le_bytes());
    }
    out.extend_from_slice(data);
    out
}
