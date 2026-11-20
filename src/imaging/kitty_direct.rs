//! Parallel `rayon`-accelerated inline Kitty graphics protocol (`t=d` / `o=z,t=d`).
//!
//! Replaces `ratatui-image`'s single-threaded RGBA8 Kitty direct transmitter with:
//! - Automatic `f=24` (RGB8) selection for opaque images (25% smaller payload and Base64)
//!   and `f=32` (RGBA8) for images with transparency.
//! - Optional fast zlib compression (`o=z`) for remote SSH sessions.
//! - Zero-intermediate-allocation parallel `rayon` + SIMD (`base64-simd`) 4096-character Base64 chunk formatting.
//! - Shared Kitty Unicode placeholder (`U+10EEEE`) rendering via [`super::kitty_shm::render_kitty_placeholders`].

use std::borrow::Cow;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use flate2::Compression;
use flate2::write::ZlibEncoder;
use image::DynamicImage;
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::style::Color;
use rayon::prelude::*;

use super::kitty_shm::{next_kitty_id, render_kitty_placeholders};

/// Maximum raw byte count per Kitty direct chunk (`3072` raw bytes -> `4096` Base64 ASCII chars).
const CHUNK_SIZE: usize = 3072;

/// Base64 ASCII length of a full `CHUNK_SIZE` raw byte chunk.
const FULL_CHUNK_B64_LEN: usize = (CHUNK_SIZE / 3) * 4;

/// Minimum chunk count before Base64 chunk formatting uses `rayon` parallelism.
const PARALLEL_CHUNK_THRESHOLD: usize = 16;

/// Parallel `rayon`-accelerated inline Kitty graphics protocol (`t=d` / `o=z,t=d`) state.
pub struct KittyDirectProtocol {
    transmit_str: String,
    transmitted: AtomicBool,
    size: Size,
    id_extra: u16,
    fg: Color,
}

impl KittyDirectProtocol {
    /// Creates a new `KittyDirectProtocol` from a canvas image and target terminal cell dimensions.
    ///
    /// If `canvas` dimensions already match `size * font_size`, avoids any intermediate resize;
    /// otherwise crops/pads via [`super::protocol::normalize_canvas_to_cells`].
    ///
    /// Uses `f=24` (RGB8) for opaque images and `f=32` (RGBA8) for transparent images.
    /// When `compress` is `true`, compresses pixel bytes with fast zlib (`o=z`) before Base64 chunking.
    pub fn new(
        canvas: &DynamicImage,
        font_size: super::picker::FontSize,
        size: Size,
        is_tmux: bool,
        compress: bool,
    ) -> Result<Self, String> {
        if size.width == 0 || size.height == 0 {
            return Err("Target size has zero width or height".to_string());
        }

        let source = super::protocol::normalize_canvas_to_cells(canvas, font_size, size);
        let source = source.as_ref();

        let width = source.width();
        let height = source.height();
        if width == 0 || height == 0 {
            return Err("Source image has zero width or height".to_string());
        }

        let (raw_pixels, pixel_fmt): (Cow<'_, [u8]>, u8) = match source {
            DynamicImage::ImageRgb8(rgb) => (Cow::Borrowed(rgb.as_raw().as_slice()), 24),
            DynamicImage::ImageRgba8(rgba) => {
                if super::types::is_rgba8_opaque(rgba.as_raw()) {
                    (
                        Cow::Owned(super::types::pack_rgba32_to_rgb24(rgba.as_raw())),
                        24,
                    )
                } else {
                    (Cow::Borrowed(rgba.as_raw().as_slice()), 32)
                }
            }
            other => {
                let rgba = super::types::to_rgba8_fast(other);
                if super::types::is_rgba8_opaque(rgba.as_raw()) {
                    (
                        Cow::Owned(super::types::pack_rgba32_to_rgb24(rgba.as_raw())),
                        24,
                    )
                } else {
                    (Cow::Owned(rgba.into_owned().into_raw()), 32)
                }
            }
        };

        let payload: Cow<'_, [u8]> = if compress {
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
            encoder
                .write_all(&raw_pixels)
                .map_err(|e| format!("Kitty zlib write failed: {e}"))?;
            let compressed = encoder
                .finish()
                .map_err(|e| format!("Kitty zlib finish failed: {e}"))?;
            Cow::Owned(compressed)
        } else {
            raw_pixels
        };

        let id = next_kitty_id();
        let [id_extra, id_r, id_g, id_b] = id.to_be_bytes();
        let fg = Color::Rgb(id_r, id_g, id_b);

        let transmit_str = format_kitty_direct_chunks(
            &payload,
            id,
            pixel_fmt,
            width,
            height,
            is_tmux,
            compress,
            PARALLEL_CHUNK_THRESHOLD,
        );

        Ok(Self {
            transmit_str,
            transmitted: AtomicBool::new(false),
            size,
            id_extra: u16::from(id_extra),
            fg,
        })
    }

    /// Returns a reference to the formatted Kitty direct transmission escape sequence.
    #[cfg(test)]
    pub fn transmit_str(&self) -> &str {
        &self.transmit_str
    }

    /// Renders the Kitty Unicode placeholder grid, prepending the transmission escape sequence
    /// only on the first render call.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let is_first_render = !self.transmitted.swap(true, Ordering::SeqCst);
        let transmit = if is_first_render {
            Some(self.transmit_str.as_str())
        } else {
            None
        };
        render_kitty_placeholders(area, buf, self.size, self.fg, self.id_extra, transmit);
    }
}

/// Encodes `src` into RFC 4648 standard Base64 directly into `dst` using runtime-dispatched SIMD.
///
/// Caller must ensure `dst.len() == src.len().div_ceil(3) * 4`.
#[inline]
fn encode_base64_slice(src: &[u8], dst: &mut [u8]) {
    if src.is_empty() {
        return;
    }
    let _ = base64_simd::STANDARD.encode(src, base64_simd::Out::from_slice(dst));
}

/// Writes a single formatted Kitty direct chunk (`header + base64(src) + trailer`) into `dst`.
#[inline]
fn write_kitty_chunk(header: &[u8], src: &[u8], trailer: &[u8], dst: &mut [u8]) {
    let h_len = header.len();
    let b64_len = src.len().div_ceil(3) * 4;
    dst[..h_len].copy_from_slice(header);
    encode_base64_slice(src, &mut dst[h_len..h_len + b64_len]);
    dst[h_len + b64_len..].copy_from_slice(trailer);
}

/// Formats `payload` into Kitty direct (`t=d`) Base64 chunks of at most 4096 Base64 characters.
///
/// Allocates a single exact-size output buffer and formats middle chunks in parallel across
/// `rayon` workers when `chunk_count >= parallel_threshold`.
#[allow(clippy::too_many_arguments)]
fn format_kitty_direct_chunks(
    payload: &[u8],
    id: u32,
    pixel_fmt: u8,
    width: u32,
    height: u32,
    is_tmux: bool,
    compress: bool,
    parallel_threshold: usize,
) -> String {
    let (start, escape, end) = super::picker::tmux_start_escape_end(is_tmux);
    let compression = if compress { "o=z," } else { "" };
    let trailer = format!("{escape}\\{end}");
    let trailer_bytes = trailer.as_bytes();

    let chunk_count = payload.len().div_ceil(CHUNK_SIZE);
    if chunk_count <= 1 {
        let header0 = format!(
            "{start}{escape}_Gq=2,i={id},a=T,U=1,f={pixel_fmt},{compression}t=d,s={width},v={height},m=0;"
        );
        let b64_len = payload.len().div_ceil(3) * 4;
        let total_len = header0.len() + b64_len + trailer_bytes.len();
        let mut out = vec![0u8; total_len];
        write_kitty_chunk(header0.as_bytes(), payload, trailer_bytes, &mut out);
        return String::from_utf8(out).expect("Kitty direct chunk output is valid ASCII");
    }

    let header0 = format!(
        "{start}{escape}_Gq=2,i={id},a=T,U=1,f={pixel_fmt},{compression}t=d,s={width},v={height},m=1;"
    );
    let mid_header = format!("{start}{escape}_Gq=2,m=1;");
    let last_header = format!("{start}{escape}_Gq=2,m=0;");

    let chunk0_len = header0.len() + FULL_CHUNK_B64_LEN + trailer_bytes.len();
    let mid_count = chunk_count - 2;
    let mid_chunk_len = mid_header.len() + FULL_CHUNK_B64_LEN + trailer_bytes.len();
    let mid_total_len = mid_count * mid_chunk_len;

    let last_src = &payload[(chunk_count - 1) * CHUNK_SIZE..];
    let last_b64_len = last_src.len().div_ceil(3) * 4;
    let last_chunk_len = last_header.len() + last_b64_len + trailer_bytes.len();

    let total_len = chunk0_len + mid_total_len + last_chunk_len;
    let mut out = vec![0u8; total_len];

    // Chunk 0
    write_kitty_chunk(
        header0.as_bytes(),
        &payload[..CHUNK_SIZE],
        trailer_bytes,
        &mut out[..chunk0_len],
    );

    // Middle chunks (1..chunk_count - 1)
    if mid_count > 0 {
        let mid_src = &payload[CHUNK_SIZE..(chunk_count - 1) * CHUNK_SIZE];
        let mid_dst = &mut out[chunk0_len..chunk0_len + mid_total_len];
        let mid_header_bytes = mid_header.as_bytes();

        if chunk_count >= parallel_threshold {
            mid_src
                .par_chunks(CHUNK_SIZE)
                .zip(mid_dst.par_chunks_mut(mid_chunk_len))
                .for_each(|(src_chunk, dst_chunk)| {
                    write_kitty_chunk(mid_header_bytes, src_chunk, trailer_bytes, dst_chunk);
                });
        } else {
            mid_src
                .chunks(CHUNK_SIZE)
                .zip(mid_dst.chunks_mut(mid_chunk_len))
                .for_each(|(src_chunk, dst_chunk)| {
                    write_kitty_chunk(mid_header_bytes, src_chunk, trailer_bytes, dst_chunk);
                });
        }
    }

    // Final chunk
    write_kitty_chunk(
        last_header.as_bytes(),
        last_src,
        trailer_bytes,
        &mut out[chunk0_len + mid_total_len..],
    );

    String::from_utf8(out).expect("Kitty direct chunk output is valid ASCII")
}

#[cfg(test)]
mod tests {
    use super::super::picker::{FontSize, tmux_start_escape_end};
    use super::*;
    use flate2::read::ZlibDecoder;
    use image::{GrayImage, RgbImage, RgbaImage};
    use std::io::Read;

    /// Decodes standard RFC 4648 Base64 ASCII into raw bytes for test verification.
    fn decode_base64_ascii(b64: &str) -> Vec<u8> {
        let val = |c: u8| -> u8 {
            match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => panic!("Invalid base64 char: {}", c as char),
            }
        };
        let bytes = b64.as_bytes();
        assert_eq!(bytes.len() % 4, 0);
        let mut out = Vec::with_capacity((bytes.len() / 4) * 3);
        for chunk in bytes.as_chunks::<4>().0 {
            let a = val(chunk[0]);
            let b = val(chunk[1]);
            out.push((a << 2) | (b >> 4));
            if chunk[2] != b'=' {
                let c = val(chunk[2]);
                out.push((b << 4) | (c >> 2));
                if chunk[3] != b'=' {
                    let d = val(chunk[3]);
                    out.push((c << 6) | d);
                }
            }
        }
        out
    }

    /// Parses Kitty APC chunks from `transmit_str` (for `is_tmux = false` or `true`),
    /// returning `(header_params, b64_payload)` pairs.
    fn parse_kitty_chunks(transmit_str: &str, is_tmux: bool) -> Vec<(String, String)> {
        let (start, escape, end) = tmux_start_escape_end(is_tmux);
        let prefix = format!("{start}{escape}_G");
        let suffix = format!("{escape}\\{end}");
        let mut rest = transmit_str;
        let mut chunks = Vec::new();
        while !rest.is_empty() {
            assert!(
                rest.starts_with(&prefix),
                "Chunk does not start with expected prefix"
            );
            rest = &rest[prefix.len()..];
            let end_idx = rest
                .find(&suffix)
                .expect("Chunk missing expected terminator");
            let body = &rest[..end_idx];
            let (params, b64) = body.split_once(';').expect("Chunk missing ';' separator");
            chunks.push((params.to_string(), b64.to_string()));
            rest = &rest[end_idx + suffix.len()..];
        }
        chunks
    }

    #[test]
    fn test_kitty_direct_rejects_zero_dimensions() {
        let img = DynamicImage::ImageRgb8(RgbImage::new(8, 8));
        let font = FontSize {
            width: 8,
            height: 8,
        };
        assert!(KittyDirectProtocol::new(&img, font, Size::new(0, 1), false, false).is_err());
        assert!(KittyDirectProtocol::new(&img, font, Size::new(1, 0), false, false).is_err());
    }

    #[test]
    fn test_kitty_direct_format_selection_opaque_vs_transparent() {
        let font = FontSize {
            width: 2,
            height: 2,
        };
        let size = Size::new(2, 2); // 4x4 pixels

        // 1. Opaque RGB8 -> f=24
        let rgb_raw: Vec<u8> = (0..4 * 4 * 3).map(|i| (i * 7 % 256) as u8).collect();
        let rgb_img = DynamicImage::ImageRgb8(RgbImage::from_raw(4, 4, rgb_raw.clone()).unwrap());
        let proto_rgb = KittyDirectProtocol::new(&rgb_img, font, size, false, false).unwrap();
        let chunks = parse_kitty_chunks(proto_rgb.transmit_str(), false);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].0.contains("f=24"));
        assert!(chunks[0].0.contains("t=d"));
        assert!(!chunks[0].0.contains("o=z"));
        assert!(chunks[0].0.contains("s=4,v=4,m=0"));
        assert_eq!(decode_base64_ascii(&chunks[0].1), rgb_raw);

        // 2. Opaque RGBA8 -> packed to RGB8 with f=24
        let mut opaque_rgba_raw = Vec::with_capacity(4 * 4 * 4);
        for px in rgb_raw.as_chunks::<3>().0 {
            opaque_rgba_raw.extend_from_slice(&[px[0], px[1], px[2], 255]);
        }
        let opaque_rgba =
            DynamicImage::ImageRgba8(RgbaImage::from_raw(4, 4, opaque_rgba_raw).unwrap());
        let proto_opaque =
            KittyDirectProtocol::new(&opaque_rgba, font, size, false, false).unwrap();
        let chunks = parse_kitty_chunks(proto_opaque.transmit_str(), false);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].0.contains("f=24"));
        assert_eq!(decode_base64_ascii(&chunks[0].1), rgb_raw);

        // 3. Transparent RGBA8 -> f=32
        let mut trans_rgba_raw = vec![200u8; 4 * 4 * 4];
        trans_rgba_raw[3] = 128; // semi-transparent first pixel
        let trans_rgba =
            DynamicImage::ImageRgba8(RgbaImage::from_raw(4, 4, trans_rgba_raw.clone()).unwrap());
        let proto_trans = KittyDirectProtocol::new(&trans_rgba, font, size, false, false).unwrap();
        let chunks = parse_kitty_chunks(proto_trans.transmit_str(), false);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].0.contains("f=32"));
        assert_eq!(decode_base64_ascii(&chunks[0].1), trans_rgba_raw);

        // 4. Other image variant (Grayscale Luma8) -> opaque, packed to f=24
        let gray = DynamicImage::ImageLuma8(GrayImage::from_raw(4, 4, vec![42u8; 16]).unwrap());
        let proto_gray = KittyDirectProtocol::new(&gray, font, size, false, false).unwrap();
        let chunks = parse_kitty_chunks(proto_gray.transmit_str(), false);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].0.contains("f=24"));
        assert_eq!(decode_base64_ascii(&chunks[0].1), vec![42u8; 4 * 4 * 3]);
    }

    #[test]
    fn test_kitty_direct_compressed_zlib_round_trip() {
        let font = FontSize {
            width: 8,
            height: 8,
        };
        let size = Size::new(4, 4); // 32x32 pixels

        // Opaque RGB8 compressed (o=z, f=24)
        let rgb_raw: Vec<u8> = (0..32 * 32 * 3).map(|i| ((i / 3) % 64) as u8).collect();
        let rgb_img = DynamicImage::ImageRgb8(RgbImage::from_raw(32, 32, rgb_raw.clone()).unwrap());
        let proto_rgb = KittyDirectProtocol::new(&rgb_img, font, size, false, true).unwrap();
        let chunks = parse_kitty_chunks(proto_rgb.transmit_str(), false);
        assert!(chunks[0].0.contains("f=24,o=z,t=d,s=32,v=32"));

        let mut reassembled = Vec::new();
        for (_, b64) in &chunks {
            reassembled.extend_from_slice(&decode_base64_ascii(b64));
        }
        let mut decoder = ZlibDecoder::new(reassembled.as_slice());
        let mut inflated = Vec::new();
        decoder.read_to_end(&mut inflated).unwrap();
        assert_eq!(inflated, rgb_raw);

        // Transparent RGBA8 compressed (o=z, f=32)
        let mut rgba_raw = vec![100u8; 32 * 32 * 4];
        rgba_raw[7] = 50;
        let rgba_img =
            DynamicImage::ImageRgba8(RgbaImage::from_raw(32, 32, rgba_raw.clone()).unwrap());
        let proto_rgba = KittyDirectProtocol::new(&rgba_img, font, size, false, true).unwrap();
        let chunks = parse_kitty_chunks(proto_rgba.transmit_str(), false);
        assert!(chunks[0].0.contains("f=32,o=z,t=d,s=32,v=32"));

        let mut reassembled = Vec::new();
        for (_, b64) in &chunks {
            reassembled.extend_from_slice(&decode_base64_ascii(b64));
        }
        let mut decoder = ZlibDecoder::new(reassembled.as_slice());
        let mut inflated = Vec::new();
        decoder.read_to_end(&mut inflated).unwrap();
        assert_eq!(inflated, rgba_raw);
    }

    #[test]
    fn test_kitty_direct_parallel_multichunk_and_tmux_framing() {
        // 160x160 RGB8 = 76,800 raw bytes -> 25 chunks (>= 16, exercises rayon parallel path!)
        // Plus add a non-multiple-of-3072 test (161x160 = 77,280 -> 26 chunks with partial last chunk)
        let width = 161u32;
        let height = 160u32;
        let rgb_raw: Vec<u8> = (0..(width * height * 3) as usize)
            .map(|i| ((i * 31 + 17) % 256) as u8)
            .collect();
        let img =
            DynamicImage::ImageRgb8(RgbImage::from_raw(width, height, rgb_raw.clone()).unwrap());
        let font = FontSize {
            width: 1,
            height: 1,
        };
        let size = Size::new(width as u16, height as u16);

        for is_tmux in [false, true] {
            let proto = KittyDirectProtocol::new(&img, font, size, is_tmux, false).unwrap();
            let chunks = parse_kitty_chunks(proto.transmit_str(), is_tmux);

            let expected_chunks = rgb_raw.len().div_ceil(CHUNK_SIZE);
            assert!(
                expected_chunks >= PARALLEL_CHUNK_THRESHOLD,
                "Test must exercise parallel rayon path"
            );
            assert_eq!(chunks.len(), expected_chunks);

            let mut reassembled = Vec::with_capacity(rgb_raw.len());
            for (idx, (params, b64)) in chunks.iter().enumerate() {
                assert!(
                    b64.len() <= 4096,
                    "Chunk {idx} exceeded 4096 Base64 chars: {}",
                    b64.len()
                );
                if idx == 0 {
                    assert!(params.contains("a=T,U=1,f=24,t=d,s=161,v=160,m=1"));
                    assert_eq!(b64.len(), 4096);
                } else if idx + 1 < chunks.len() {
                    assert_eq!(params, "q=2,m=1");
                    assert_eq!(b64.len(), 4096);
                } else {
                    assert_eq!(params, "q=2,m=0");
                }
                reassembled.extend_from_slice(&decode_base64_ascii(b64));
            }
            assert_eq!(reassembled, rgb_raw);
        }

        // Verify bit-for-bit equivalence between parallel and sequential paths
        let parallel_out =
            format_kitty_direct_chunks(&rgb_raw, 555, 24, width, height, false, false, 16);
        let sequential_out =
            format_kitty_direct_chunks(&rgb_raw, 555, 24, width, height, false, false, usize::MAX);
        assert_eq!(parallel_out, sequential_out);
    }

    #[test]
    fn test_kitty_direct_oneshot_render_and_placeholders() {
        let raw = vec![128u8; 4 * 4 * 3];
        let img = DynamicImage::ImageRgb8(RgbImage::from_raw(4, 4, raw).unwrap());
        let font = FontSize {
            width: 1,
            height: 1,
        };
        let size = Size::new(4, 4);
        let proto = KittyDirectProtocol::new(&img, font, size, false, false).unwrap();

        let area = Rect::new(0, 0, 4, 4);
        let mut buf1 = Buffer::empty(area);
        proto.render(area, &mut buf1);

        // First render embeds transmission sequence in cell (0, 0) followed by placeholder + diacritics
        let c00 = &buf1[(0, 0)];
        assert!(c00.symbol().contains("_Gq=2"));
        assert!(c00.symbol().contains('\u{10EEEE}'));

        // Subsequent column inherits position without diacritics
        assert_eq!(buf1[(1, 0)].symbol(), "\u{10EEEE}");
        // Row 1 col 0 has placeholder + row/col/extra diacritics
        assert!(buf1[(0, 1)].symbol().starts_with('\u{10EEEE}'));
        assert!(buf1[(0, 1)].symbol().chars().count() == 4);

        // Second render must NOT re-emit transmission sequence
        let mut buf2 = Buffer::empty(area);
        proto.render(area, &mut buf2);
        let c00_second = &buf2[(0, 0)];
        assert!(!c00_second.symbol().contains("_Gq=2"));
        assert!(c00_second.symbol().starts_with('\u{10EEEE}'));
        assert!(c00_second.symbol().chars().count() == 4);
    }

    #[test]
    fn test_kitty_direct_empty_payload_defensive() {
        let seq = format_kitty_direct_chunks(&[], 42, 24, 1, 1, false, false, 16);
        let chunks = parse_kitty_chunks(&seq, false);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].0, "q=2,i=42,a=T,U=1,f=24,t=d,s=1,v=1,m=0");
        assert!(chunks[0].1.is_empty());
    }

    #[test]
    fn test_encode_base64_slice_rfc4648_remainders() {
        let cases: &[(&[u8], &str)] = &[
            (b"", ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"fooba", "Zm9vYmE="),
            (b"foobar", "Zm9vYmFy"),
        ];
        for &(input, expected) in cases {
            let mut dst = vec![0u8; input.len().div_ceil(3) * 4];
            encode_base64_slice(input, &mut dst);
            assert_eq!(
                std::str::from_utf8(&dst).unwrap(),
                expected,
                "Mismatch for input len {}",
                input.len()
            );
            assert_eq!(decode_base64_ascii(expected), input);
        }
    }

    #[test]
    fn test_kitty_direct_chunk_boundaries_and_parallel_threshold() {
        // Covers:
        // - exact 1 chunk (3072 bytes)
        // - 2 chunks with mid_count == 0 (3073 bytes with == padding, 3074 bytes with = padding, exact 6144 bytes)
        // - 15 chunks (just below PARALLEL_CHUNK_THRESHOLD = 16, sequential middle path)
        // - 16 chunks (at PARALLEL_CHUNK_THRESHOLD = 16, parallel middle path, both exact multiple and partial)
        let sizes = [
            CHUNK_SIZE,
            CHUNK_SIZE + 1,
            CHUNK_SIZE + 2,
            2 * CHUNK_SIZE,
            14 * CHUNK_SIZE + 5,
            15 * CHUNK_SIZE,
            15 * CHUNK_SIZE + 1,
            16 * CHUNK_SIZE,
        ];

        for &len in &sizes {
            let payload: Vec<u8> = (0..len).map(|i| ((i * 73 + 19) % 256) as u8).collect();
            let actual = format_kitty_direct_chunks(
                &payload,
                999,
                24,
                16,
                16,
                false,
                false,
                PARALLEL_CHUNK_THRESHOLD,
            );
            let reference =
                format_kitty_direct_chunks(&payload, 999, 24, 16, 16, false, false, usize::MAX);
            assert_eq!(
                actual, reference,
                "Parallel/threshold output diverged from sequential reference for len={len}"
            );

            let chunks = parse_kitty_chunks(&actual, false);
            let expected_chunks = len.div_ceil(CHUNK_SIZE);
            assert_eq!(
                chunks.len(),
                expected_chunks,
                "Wrong chunk count for len={len}"
            );

            let mut reassembled = Vec::with_capacity(len);
            for (idx, (params, b64)) in chunks.iter().enumerate() {
                assert!(b64.len() <= 4096);
                let is_last = idx + 1 == chunks.len();
                if idx == 0 {
                    let expected_more = if is_last { "m=0" } else { "m=1" };
                    assert!(
                        params.ends_with(expected_more),
                        "Chunk 0 for len={len} expected {expected_more}, got {params}"
                    );
                } else {
                    let expected_params = if is_last { "q=2,m=0" } else { "q=2,m=1" };
                    assert_eq!(params, expected_params);
                }
                if !is_last || len.is_multiple_of(CHUNK_SIZE) {
                    assert_eq!(
                        b64.len(),
                        4096,
                        "Expected full 4096-char chunk for len={len}"
                    );
                }
                reassembled.extend_from_slice(&decode_base64_ascii(b64));
            }
            assert_eq!(
                reassembled, payload,
                "Payload round-trip failed for len={len}"
            );
        }
    }
}
