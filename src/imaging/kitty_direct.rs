//! Parallel `rayon`-accelerated inline Kitty graphics protocol (`t=d` / `o=z,t=d`).
//!
//! Replaces `ratatui-image`'s single-threaded RGBA8 Kitty direct transmitter with:
//! - Automatic `f=24` (RGB8) selection for opaque images (25% smaller payload and Base64)
//!   and `f=32` (RGBA8) for images with transparency.
//! - Optional fast zlib compression (`o=z`) for remote SSH sessions.
//! - Zero-`unsafe`, zero-pre-zero-fill parallel `rayon` + SIMD (`base64-simd`) 4096-character Base64 chunk formatting.
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
    transmit_slabs: Vec<Vec<u8>>,
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

        let transmit_slabs = format_kitty_direct_chunks(
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
            transmit_slabs,
            transmitted: AtomicBool::new(false),
            size,
            id_extra: u16::from(id_extra),
            fg,
        })
    }

    /// Returns the formatted Kitty direct transmission escape sequence.
    ///
    /// Borrows directly when stored in a single slab, or concatenates slabs for multi-slab
    /// parallel outputs.
    #[cfg(test)]
    pub fn transmit_str(&self) -> Cow<'_, str> {
        if self.transmit_slabs.len() == 1 {
            Cow::Borrowed(
                std::str::from_utf8(&self.transmit_slabs[0])
                    .expect("Kitty direct chunk output is valid ASCII"),
            )
        } else {
            let total_len: usize = self.transmit_slabs.iter().map(Vec::len).sum();
            let mut flat = Vec::with_capacity(total_len);
            for slab in &self.transmit_slabs {
                flat.extend_from_slice(slab);
            }
            Cow::Owned(String::from_utf8(flat).expect("Kitty direct chunk output is valid ASCII"))
        }
    }

    /// Writes the one-shot Kitty direct transmission escape sequence directly to `writer`
    /// if it has not yet been transmitted.
    ///
    /// Returns `Ok(true)` if the transmission bytes were written, or `Ok(false)` if they
    /// were already transmitted. If `writer.write_all` fails, the transmitted flag remains
    /// unset so a subsequent attempt can retry.
    pub fn write_pending_transmit(&self, mut writer: impl Write) -> std::io::Result<bool> {
        if self.transmitted.load(Ordering::Acquire) {
            return Ok(false);
        }
        for slab in &self.transmit_slabs {
            writer.write_all(slab)?;
        }
        self.transmitted.store(true, Ordering::Release);
        Ok(true)
    }

    /// Renders the Kitty Unicode placeholder grid, prepending the transmission escape sequence
    /// only on the first render call if not already transmitted via [`Self::write_pending_transmit`].
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let is_first_render = !self.transmitted.swap(true, Ordering::SeqCst);
        let owned_fallback;
        let transmit = if is_first_render {
            if self.transmit_slabs.len() == 1 {
                std::str::from_utf8(&self.transmit_slabs[0]).ok()
            } else {
                let total_len: usize = self.transmit_slabs.iter().map(Vec::len).sum();
                let mut flat = Vec::with_capacity(total_len);
                for slab in &self.transmit_slabs {
                    flat.extend_from_slice(slab);
                }
                owned_fallback = flat;
                std::str::from_utf8(&owned_fallback).ok()
            }
        } else {
            None
        };
        render_kitty_placeholders(area, buf, self.size, self.fg, self.id_extra, transmit);
    }
}

/// Appends a single formatted Kitty direct chunk (`header + base64(src) + trailer`) to `dst`
/// using runtime-dispatched SIMD Base64 encoding without intermediate allocations.
#[inline]
fn append_kitty_chunk(header: &[u8], src: &[u8], trailer: &[u8], dst: &mut Vec<u8>) {
    dst.extend_from_slice(header);
    if !src.is_empty() {
        base64_simd::STANDARD.encode_append(src, dst);
    }
    dst.extend_from_slice(trailer);
}

/// Formats `payload` into Kitty direct (`t=d`) Base64 chunks of at most 4096 Base64 characters.
///
/// Returns one or more pure-ASCII byte slabs whose concatenation is the complete Kitty direct
/// transmission sequence. When `chunk_count >= parallel_threshold`, partitions chunks across
/// `rayon` workers into pre-sized slabs without `unsafe` or pre-zero-fill allocations.
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
) -> Vec<Vec<u8>> {
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
        let mut out = Vec::with_capacity(total_len);
        append_kitty_chunk(header0.as_bytes(), payload, trailer_bytes, &mut out);
        debug_assert_eq!(out.len(), total_len);
        return vec![out];
    }

    let header0 = format!(
        "{start}{escape}_Gq=2,i={id},a=T,U=1,f={pixel_fmt},{compression}t=d,s={width},v={height},m=1;"
    );
    let mid_header = format!("{start}{escape}_Gq=2,m=1;");
    let last_header = format!("{start}{escape}_Gq=2,m=0;");
    let header0_bytes = header0.as_bytes();
    let mid_header_bytes = mid_header.as_bytes();
    let last_header_bytes = last_header.as_bytes();

    let chunk0_len = header0_bytes.len() + FULL_CHUNK_B64_LEN + trailer_bytes.len();
    let mid_count = chunk_count - 2;
    let mid_chunk_len = mid_header_bytes.len() + FULL_CHUNK_B64_LEN + trailer_bytes.len();

    let last_src = &payload[(chunk_count - 1) * CHUNK_SIZE..];
    let last_b64_len = last_src.len().div_ceil(3) * 4;
    let last_chunk_len = last_header_bytes.len() + last_b64_len + trailer_bytes.len();

    if chunk_count < parallel_threshold {
        let total_len = chunk0_len + mid_count * mid_chunk_len + last_chunk_len;
        let mut out = Vec::with_capacity(total_len);
        append_kitty_chunk(
            header0_bytes,
            &payload[..CHUNK_SIZE],
            trailer_bytes,
            &mut out,
        );
        for chunk_idx in 1..(chunk_count - 1) {
            let byte_start = chunk_idx * CHUNK_SIZE;
            append_kitty_chunk(
                mid_header_bytes,
                &payload[byte_start..byte_start + CHUNK_SIZE],
                trailer_bytes,
                &mut out,
            );
        }
        append_kitty_chunk(last_header_bytes, last_src, trailer_bytes, &mut out);
        debug_assert_eq!(out.len(), total_len);
        return vec![out];
    }

    let target_slabs = rayon::current_num_threads().max(1) * 2;
    let chunks_per_slab = chunk_count.div_ceil(target_slabs).max(1);
    let slab_count = chunk_count.div_ceil(chunks_per_slab);

    (0..slab_count)
        .into_par_iter()
        .map(|slab_idx| {
            let start_chunk = slab_idx * chunks_per_slab;
            let end_chunk = (start_chunk + chunks_per_slab).min(chunk_count);

            let mut slab_cap = 0usize;
            for chunk_idx in start_chunk..end_chunk {
                slab_cap += if chunk_idx == 0 {
                    chunk0_len
                } else if chunk_idx + 1 == chunk_count {
                    last_chunk_len
                } else {
                    mid_chunk_len
                };
            }

            let mut slab = Vec::with_capacity(slab_cap);
            for chunk_idx in start_chunk..end_chunk {
                let byte_start = chunk_idx * CHUNK_SIZE;
                let byte_end = (byte_start + CHUNK_SIZE).min(payload.len());
                let header = if chunk_idx == 0 {
                    header0_bytes
                } else if chunk_idx + 1 == chunk_count {
                    last_header_bytes
                } else {
                    mid_header_bytes
                };
                append_kitty_chunk(
                    header,
                    &payload[byte_start..byte_end],
                    trailer_bytes,
                    &mut slab,
                );
            }
            debug_assert_eq!(slab.len(), slab_cap);
            slab
        })
        .collect()
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

    /// Flattens transmission slabs into a single contiguous byte buffer for test assertions.
    fn flatten_slabs(slabs: &[Vec<u8>]) -> Vec<u8> {
        let total_len: usize = slabs.iter().map(Vec::len).sum();
        let mut flat = Vec::with_capacity(total_len);
        for slab in slabs {
            flat.extend_from_slice(slab);
        }
        flat
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
        let chunks = parse_kitty_chunks(&proto_rgb.transmit_str(), false);
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
        let chunks = parse_kitty_chunks(&proto_opaque.transmit_str(), false);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].0.contains("f=24"));
        assert_eq!(decode_base64_ascii(&chunks[0].1), rgb_raw);

        // 3. Transparent RGBA8 -> f=32
        let mut trans_rgba_raw = vec![200u8; 4 * 4 * 4];
        trans_rgba_raw[3] = 128; // semi-transparent first pixel
        let trans_rgba =
            DynamicImage::ImageRgba8(RgbaImage::from_raw(4, 4, trans_rgba_raw.clone()).unwrap());
        let proto_trans = KittyDirectProtocol::new(&trans_rgba, font, size, false, false).unwrap();
        let chunks = parse_kitty_chunks(&proto_trans.transmit_str(), false);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].0.contains("f=32"));
        assert_eq!(decode_base64_ascii(&chunks[0].1), trans_rgba_raw);

        // 4. Other image variant (Grayscale Luma8) -> opaque, packed to f=24
        let gray = DynamicImage::ImageLuma8(GrayImage::from_raw(4, 4, vec![42u8; 16]).unwrap());
        let proto_gray = KittyDirectProtocol::new(&gray, font, size, false, false).unwrap();
        let chunks = parse_kitty_chunks(&proto_gray.transmit_str(), false);
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
        let chunks = parse_kitty_chunks(&proto_rgb.transmit_str(), false);
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
        let chunks = parse_kitty_chunks(&proto_rgba.transmit_str(), false);
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
            let chunks = parse_kitty_chunks(&proto.transmit_str(), is_tmux);

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

            // Verify multi-slab write_pending_transmit matches transmit_str bit-for-bit
            let mut written = Vec::new();
            assert!(proto.write_pending_transmit(&mut written).unwrap());
            assert_eq!(written, proto.transmit_str().as_bytes());
        }

        // Verify bit-for-bit equivalence between parallel and sequential paths
        let parallel_slabs =
            format_kitty_direct_chunks(&rgb_raw, 555, 24, width, height, false, false, 16);
        let sequential_slabs =
            format_kitty_direct_chunks(&rgb_raw, 555, 24, width, height, false, false, usize::MAX);
        assert_eq!(sequential_slabs.len(), 1);
        assert!(!parallel_slabs.is_empty());
        let parallel_out = flatten_slabs(&parallel_slabs);
        let sequential_out = flatten_slabs(&sequential_slabs);
        assert!(parallel_out.is_ascii());
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

        // Multi-slab fallback path in render() (skipping write_pending_transmit on a large image)
        let large_w = 160u32;
        let large_h = 160u32;
        let large_img = DynamicImage::ImageRgb8(
            RgbImage::from_raw(
                large_w,
                large_h,
                vec![99u8; (large_w * large_h * 3) as usize],
            )
            .unwrap(),
        );
        let large_proto = KittyDirectProtocol::new(
            &large_img,
            FontSize {
                width: 40,
                height: 40,
            },
            Size::new(4, 4),
            false,
            false,
        )
        .unwrap();
        let mut large_buf = Buffer::empty(area);
        large_proto.render(area, &mut large_buf);
        let large_c00 = &large_buf[(0, 0)];
        assert!(
            large_c00
                .symbol()
                .starts_with(large_proto.transmit_str().as_ref())
        );
        assert!(large_c00.symbol().contains('\u{10EEEE}'));
    }

    #[test]
    fn test_kitty_direct_write_pending_transmit_oneshot_and_clean_render() {
        struct FailingWriter;
        impl Write for FailingWriter {
            fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "simulated write failure",
                ))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let raw = vec![64u8; 4 * 4 * 3];
        let img = DynamicImage::ImageRgb8(RgbImage::from_raw(4, 4, raw).unwrap());
        let font = FontSize {
            width: 1,
            height: 1,
        };
        let size = Size::new(4, 4);
        let proto = KittyDirectProtocol::new(&img, font, size, false, false).unwrap();

        // A failing write must NOT mark the protocol as transmitted
        assert!(proto.write_pending_transmit(FailingWriter).is_err());

        // First successful write emits exact transmit_str bytes and returns Ok(true)
        let mut out = Vec::new();
        assert!(proto.write_pending_transmit(&mut out).unwrap());
        assert_eq!(out, proto.transmit_str().as_bytes());

        // Second call is a no-op returning Ok(false)
        let mut out2 = Vec::new();
        assert!(!proto.write_pending_transmit(&mut out2).unwrap());
        assert!(out2.is_empty());

        // Subsequent render() emits ONLY clean Unicode placeholder cells (no transmit_str)
        let area = Rect::new(0, 0, 4, 4);
        let mut buf = Buffer::empty(area);
        proto.render(area, &mut buf);
        let c00 = &buf[(0, 0)];
        assert!(!c00.symbol().contains("_Gq=2"));
        assert!(c00.symbol().starts_with('\u{10EEEE}'));
        assert_eq!(c00.symbol().chars().count(), 4);
        assert_eq!(buf[(1, 0)].symbol(), "\u{10EEEE}");
    }

    #[test]
    fn test_kitty_direct_empty_payload_defensive() {
        let slabs = format_kitty_direct_chunks(&[], 42, 24, 1, 1, false, false, 16);
        assert_eq!(slabs.len(), 1);
        let seq = &slabs[0];
        assert!(seq.is_ascii());
        let seq_str = std::str::from_utf8(seq).expect("Kitty direct output must be valid ASCII");
        let chunks = parse_kitty_chunks(seq_str, false);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].0, "q=2,i=42,a=T,U=1,f=24,t=d,s=1,v=1,m=0");
        assert!(chunks[0].1.is_empty());
    }

    #[test]
    fn test_append_kitty_chunk_rfc4648_remainders() {
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
            let mut dst = Vec::new();
            append_kitty_chunk(b"<H>", input, b"<T>", &mut dst);
            assert!(dst.is_ascii());
            assert_eq!(
                std::str::from_utf8(&dst).unwrap(),
                format!("<H>{expected}<T>"),
                "Mismatch for input len {}",
                input.len()
            );
            assert_eq!(decode_base64_ascii(expected), input);
        }
    }

    #[test]
    fn test_kitty_direct_chunk_boundaries_and_parallel_threshold() {
        // Covers:
        // - 1 partial chunk with Base64 remainder cases (1, 2, 3 bytes)
        // - exact 1 chunk (3072 bytes)
        // - 2 chunks with mid_count == 0 (3073 bytes with == padding, 3074 bytes with = padding, exact 6144 bytes)
        // - 15 chunks (just below PARALLEL_CHUNK_THRESHOLD = 16, sequential middle path)
        // - 16 chunks (at PARALLEL_CHUNK_THRESHOLD = 16, parallel middle path, both exact multiple and partial)
        let sizes = [
            1,
            2,
            3,
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
            let actual_slabs = format_kitty_direct_chunks(
                &payload,
                999,
                24,
                16,
                16,
                false,
                false,
                PARALLEL_CHUNK_THRESHOLD,
            );
            let reference_slabs =
                format_kitty_direct_chunks(&payload, 999, 24, 16, 16, false, false, usize::MAX);
            assert_eq!(reference_slabs.len(), 1);
            let actual = flatten_slabs(&actual_slabs);
            let reference = flatten_slabs(&reference_slabs);
            assert_eq!(
                actual, reference,
                "Parallel/threshold output diverged from sequential reference for len={len}"
            );
            assert!(
                actual.is_ascii(),
                "Kitty direct output must be valid ASCII for len={len}"
            );

            let actual_str =
                std::str::from_utf8(&actual).expect("Kitty direct output must be valid UTF-8");
            let chunks = parse_kitty_chunks(actual_str, false);
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
