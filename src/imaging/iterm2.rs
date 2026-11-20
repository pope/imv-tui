//! Fast iTerm2 inline image protocol (`OSC 1337`) encoder and renderer.
//!
//! Replaces `ratatui-image`'s default PNG encoder (`CompressionType::Default` + `Adaptive`)
//! with `PNG Fast + Paeth` (lossless) and optional `JPEG q90` (opaque compressed mode for SSH),
//! paired with a safe parallel `rayon` + SIMD (`base64-simd`) Base64 encoder.

use std::borrow::Cow;
use std::num::NonZeroU16;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::{DynamicImage, ExtendedColorType, ImageEncoder};
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::{Rect, Size};
use rayon::prelude::*;

const UNIT_WIDTH: CellDiffOption = CellDiffOption::ForcedWidth(NonZeroU16::new(1).unwrap());

/// Minimum payload byte length (`64 KiB`) before Base64 encoding uses `rayon` parallelism.
const PARALLEL_B64_THRESHOLD: usize = 64 * 1024;

/// Number of 3-byte input triplets per parallel Base64 work chunk (`48 KiB` input -> `64 KiB` output).
const PARALLEL_B64_CHUNK_TRIPLETS: usize = 16_384;

/// JPEG quality factor used for opaque images when compressed transfer mode is active.
const ITERM2_JPEG_QUALITY: u8 = 90;

/// Fast iTerm2 inline image protocol (`OSC 1337`) state ready for direct `ratatui` buffer rendering.
pub struct Iterm2Protocol {
    data: String,
    size: Size,
}

impl Iterm2Protocol {
    /// Encodes `canvas` into an iTerm2 `OSC 1337` inline image escape sequence sized for `size`
    /// terminal cells.
    ///
    /// If `canvas` dimensions already match `size * font_size`, avoids any intermediate resize;
    /// otherwise crops/pads via [`super::protocol::normalize_canvas_to_cells`].
    ///
    /// When `use_jpeg_for_opaque` is `true` and the image has no transparent pixels, encodes
    /// as quality-90 JPEG; otherwise encodes as lossless PNG (`CompressionType::Fast` + `Paeth`).
    pub fn new(
        canvas: &DynamicImage,
        font_size: super::picker::FontSize,
        size: Size,
        is_tmux: bool,
        use_jpeg_for_opaque: bool,
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

        let encoded = encode_image_payload(source, width, height, use_jpeg_for_opaque)?;
        let data = build_osc1337_sequence(&encoded, width, height, size, is_tmux);

        Ok(Self { data, size })
    }

    /// Renders the iTerm2 escape sequence into the top-left cell of `area` and marks all
    /// remaining cells covered by `self.size` as [`CellDiffOption::Skip`].
    ///
    /// Safely no-ops if `self.size` exceeds `area`.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        if self.size.width > area.width || self.size.height > area.height {
            return;
        }
        let render_area = Rect::new(area.x, area.y, self.size.width, self.size.height);
        if let Some(cell) = buf.cell_mut((render_area.left(), render_area.top())) {
            cell.set_symbol(&self.data).set_diff_option(UNIT_WIDTH);
        }

        for y in render_area.top()..render_area.bottom() {
            for x in render_area.left()..render_area.right() {
                if x == render_area.left() && y == render_area.top() {
                    continue;
                }
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_diff_option(CellDiffOption::Skip);
                }
            }
        }
    }

    /// Returns the encoded iTerm2 escape sequence string.
    #[cfg(test)]
    pub fn data(&self) -> &str {
        &self.data
    }

    /// Returns the terminal cell dimensions occupied by this iTerm2 image.
    #[cfg(test)]
    pub fn size(&self) -> Size {
        self.size
    }
}

/// Encodes `source` into either JPEG (q90, opaque only) or fast lossless PNG (Fast + Paeth).
fn encode_image_payload(
    source: &DynamicImage,
    width: u32,
    height: u32,
    use_jpeg_for_opaque: bool,
) -> Result<Vec<u8>, String> {
    let pixel_count = (width as usize).saturating_mul(height as usize);
    let initial_cap = if use_jpeg_for_opaque {
        (pixel_count / 4).max(4096)
    } else {
        pixel_count.max(4096)
    };
    let mut encoded = Vec::with_capacity(initial_cap);

    match source {
        DynamicImage::ImageRgb8(rgb) => {
            encode_opaque_rgb8(
                rgb.as_raw(),
                width,
                height,
                use_jpeg_for_opaque,
                &mut encoded,
            )?;
        }
        DynamicImage::ImageRgba8(rgba) => {
            if super::types::is_rgba8_opaque(rgba.as_raw()) {
                let rgb = super::types::pack_rgba32_to_rgb24(rgba.as_raw());
                encode_opaque_rgb8(&rgb, width, height, use_jpeg_for_opaque, &mut encoded)?;
            } else {
                encode_png_fast(
                    rgba.as_raw(),
                    width,
                    height,
                    ExtendedColorType::Rgba8,
                    &mut encoded,
                )?;
            }
        }
        other => {
            let rgba: Cow<'_, image::RgbaImage> = super::types::to_rgba8_fast(other);
            if super::types::is_rgba8_opaque(rgba.as_raw()) {
                let rgb = super::types::pack_rgba32_to_rgb24(rgba.as_raw());
                encode_opaque_rgb8(&rgb, width, height, use_jpeg_for_opaque, &mut encoded)?;
            } else {
                encode_png_fast(
                    rgba.as_raw(),
                    width,
                    height,
                    ExtendedColorType::Rgba8,
                    &mut encoded,
                )?;
            }
        }
    }

    Ok(encoded)
}

/// Encodes opaque RGB8 pixel data as JPEG q90 or lossless PNG (`Fast` + `Paeth`).
fn encode_opaque_rgb8(
    rgb: &[u8],
    width: u32,
    height: u32,
    use_jpeg: bool,
    out: &mut Vec<u8>,
) -> Result<(), String> {
    if use_jpeg {
        let mut encoder = JpegEncoder::new_with_quality(out, ITERM2_JPEG_QUALITY);
        encoder
            .encode(rgb, width, height, ExtendedColorType::Rgb8)
            .map_err(|e| format!("JPEG encode failed: {e}"))
    } else {
        encode_png_fast(rgb, width, height, ExtendedColorType::Rgb8, out)
    }
}

/// Encodes raw pixel bytes as PNG using `CompressionType::Fast` and `FilterType::Paeth`.
fn encode_png_fast(
    raw: &[u8],
    width: u32,
    height: u32,
    color_type: ExtendedColorType,
    out: &mut Vec<u8>,
) -> Result<(), String> {
    let encoder = PngEncoder::new_with_quality(out, CompressionType::Fast, PngFilter::Paeth);
    encoder
        .write_image(raw, width, height, color_type)
        .map_err(|e| format!("PNG encode failed: {e}"))
}

/// Appends an unsigned integer as ASCII decimal digits without `fmt::Write` overhead.
#[inline]
fn push_u64(buf: &mut Vec<u8>, mut n: u64) {
    if n < 10 {
        buf.push(b'0' + n as u8);
        return;
    }
    let mut tmp = [0u8; 20];
    let mut i = 0;
    while n > 0 {
        tmp[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }
    while i > 0 {
        i -= 1;
        buf.push(tmp[i]);
    }
}

/// Emits the terminal erase-character (`ECH`) + cursor movement sequence matching `ratatui-image`.
fn write_clear_area(out: &mut Vec<u8>, escape: &str, width: u16, height: u16) {
    let esc = escape.as_bytes();
    if height == 1 {
        out.extend_from_slice(esc);
        out.push(b'[');
        push_u64(out, u64::from(width));
        out.push(b'X');
    } else {
        for _ in 0..height {
            out.extend_from_slice(esc);
            out.push(b'[');
            push_u64(out, u64::from(width));
            out.push(b'X');
            out.extend_from_slice(esc);
            out.extend_from_slice(b"[1B");
        }
        out.extend_from_slice(esc);
        out.push(b'[');
        push_u64(out, u64::from(height));
        out.push(b'A');
    }
}

/// Appends RFC 4648 standard Base64 encoding of `input` directly into `out`,
/// parallelizing SIMD encoding across `rayon` workers when `input.len() >= PARALLEL_B64_THRESHOLD`.
fn encode_base64_into(input: &[u8], out: &mut Vec<u8>) {
    encode_base64_into_with_threshold(input, out, PARALLEL_B64_THRESHOLD);
}

/// Internal Base64 helper with configurable parallel threshold for exact equivalence testing.
fn encode_base64_into_with_threshold(input: &[u8], out: &mut Vec<u8>, parallel_threshold: usize) {
    if input.is_empty() {
        return;
    }

    let start_len = out.len();
    let b64_len = input.len().div_ceil(3) * 4;
    out.resize(start_len + b64_len, 0);
    let dst = &mut out[start_len..];

    if input.len() >= parallel_threshold {
        let full_triplets = input.len() / 3;
        let full_in = &input[..full_triplets * 3];
        let full_out = &mut dst[..full_triplets * 4];

        full_in
            .par_chunks(PARALLEL_B64_CHUNK_TRIPLETS * 3)
            .zip(full_out.par_chunks_mut(PARALLEL_B64_CHUNK_TRIPLETS * 4))
            .for_each(|(src_slab, dst_slab)| {
                let _ =
                    base64_simd::STANDARD.encode(src_slab, base64_simd::Out::from_slice(dst_slab));
            });

        let rem = &input[full_triplets * 3..];
        if !rem.is_empty() {
            let rem_dst = &mut dst[full_triplets * 4..];
            let _ = base64_simd::STANDARD.encode(rem, base64_simd::Out::from_slice(rem_dst));
        }
    } else {
        let _ = base64_simd::STANDARD.encode(input, base64_simd::Out::from_slice(dst));
    }
}

/// Assembles the complete OSC 1337 iTerm2 inline image escape sequence in a single allocation.
fn build_osc1337_sequence(
    encoded: &[u8],
    px_w: u32,
    px_h: u32,
    size: Size,
    is_tmux: bool,
) -> String {
    let (start, escape, end) = super::picker::tmux_start_escape_end(is_tmux);

    let b64_len = encoded.len().div_ceil(3) * 4;
    let clear_est = usize::from(size.height) * (escape.len() * 2 + 12) + escape.len() + 10;
    let header_est = start.len() + escape.len() + 96;
    let mut out = Vec::with_capacity(header_est + clear_est + b64_len + 1 + end.len());

    out.extend_from_slice(start.as_bytes());
    write_clear_area(&mut out, escape, size.width, size.height);
    out.extend_from_slice(escape.as_bytes());
    out.extend_from_slice(b"]1337;File=inline=1;size=");
    push_u64(&mut out, encoded.len() as u64);
    out.extend_from_slice(b";width=");
    push_u64(&mut out, u64::from(px_w));
    out.extend_from_slice(b"px;height=");
    push_u64(&mut out, u64::from(px_h));
    out.extend_from_slice(b"px;doNotMoveCursor=1:");
    encode_base64_into(encoded, &mut out);
    out.push(b'\x07');
    out.extend_from_slice(end.as_bytes());

    String::from_utf8(out).expect("iTerm2 OSC 1337 sequence is valid ASCII")
}

#[cfg(test)]
mod tests {
    use super::super::picker::FontSize;
    use super::*;
    use image::{Rgb, RgbImage, Rgba, RgbaImage};

    fn unit_font() -> FontSize {
        FontSize {
            width: 1,
            height: 1,
        }
    }

    /// Simple reference RFC 4648 Base64 decoder for test payload verification.
    fn decode_base64(input: &str) -> Vec<u8> {
        let val = |b: u8| -> u8 {
            match b {
                b'A'..=b'Z' => b - b'A',
                b'a'..=b'z' => b - b'a' + 26,
                b'0'..=b'9' => b - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                _ => panic!("Invalid base64 byte: {b}"),
            }
        };
        let bytes = input.as_bytes();
        assert_eq!(bytes.len() % 4, 0, "Base64 length must be a multiple of 4");
        let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
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

    /// Extracts and decodes the raw binary image payload from an OSC 1337 sequence.
    fn extract_payload_bytes(seq: &str) -> Vec<u8> {
        let marker = "doNotMoveCursor=1:";
        let start = seq
            .find(marker)
            .expect("Missing OSC 1337 doNotMoveCursor=1: header")
            + marker.len();
        let bell = seq[start..]
            .find('\x07')
            .expect("Missing BEL terminator in OSC 1337 sequence");
        decode_base64(&seq[start..start + bell])
    }

    #[test]
    fn test_base64_rfc4648_vectors_and_parallel_equivalence() {
        // RFC 4648 Section 10 test vectors
        for (input, expected) in [
            (b"".as_slice(), ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"fooba", "Zm9vYmE="),
            (b"foobar", "Zm9vYmFy"),
        ] {
            let mut out = Vec::new();
            encode_base64_into(input, &mut out);
            assert_eq!(std::str::from_utf8(&out).unwrap(), expected);
        }

        // Exact equivalence between sequential and parallel across edge lengths and >= 64 KiB
        for len in [
            0usize, 1, 2, 3, 4, 5, 255, 256, 65_535, 65_536, 65_537, 65_538, 150_000, 150_001,
            150_002,
        ] {
            let input: Vec<u8> = (0..len).map(|i| ((i * 37 + 13) & 0xFF) as u8).collect();
            let mut seq_out = Vec::new();
            let mut par_out = Vec::new();
            encode_base64_into_with_threshold(&input, &mut seq_out, usize::MAX);
            encode_base64_into_with_threshold(&input, &mut par_out, 0);
            assert_eq!(seq_out, par_out, "Mismatch at length {len}");

            let decoded = decode_base64(std::str::from_utf8(&par_out).unwrap());
            assert_eq!(decoded, input, "Round-trip mismatch at length {len}");
        }
    }

    #[test]
    fn test_iterm2_opaque_rgb8_and_rgba8_png_mode() {
        let mut rgb = RgbImage::new(16, 12);
        for (x, y, p) in rgb.enumerate_pixels_mut() {
            *p = Rgb([(x * 16) as u8, (y * 20) as u8, 128]);
        }
        let dyn_rgb = DynamicImage::ImageRgb8(rgb.clone());
        let proto_rgb =
            Iterm2Protocol::new(&dyn_rgb, unit_font(), Size::new(16, 12), false, false).unwrap();

        let payload_rgb = extract_payload_bytes(proto_rgb.data());
        assert_eq!(
            &payload_rgb[..8],
            b"\x89PNG\r\n\x1a\n",
            "Expected PNG magic bytes for RGB8 in PNG mode"
        );
        let decoded_rgb = image::load_from_memory(&payload_rgb).unwrap().to_rgb8();
        assert_eq!(decoded_rgb, rgb);

        // Opaque RGBA8 should pack to RGB8 and still produce valid lossless PNG
        let mut rgba = RgbaImage::new(16, 12);
        for (x, y, p) in rgba.enumerate_pixels_mut() {
            let px = rgb.get_pixel(x, y);
            *p = Rgba([px[0], px[1], px[2], 255]);
        }
        let dyn_rgba = DynamicImage::ImageRgba8(rgba);
        let proto_rgba =
            Iterm2Protocol::new(&dyn_rgba, unit_font(), Size::new(16, 12), false, false).unwrap();
        let payload_rgba = extract_payload_bytes(proto_rgba.data());
        assert_eq!(&payload_rgba[..8], b"\x89PNG\r\n\x1a\n");
        let decoded_rgba = image::load_from_memory(&payload_rgba).unwrap().to_rgb8();
        assert_eq!(decoded_rgba, rgb);
    }

    #[test]
    fn test_iterm2_opaque_jpeg_mode_emits_jpeg_soi() {
        let mut rgb = RgbImage::new(32, 24);
        for (x, y, p) in rgb.enumerate_pixels_mut() {
            *p = Rgb([(x * 7) as u8, (y * 10) as u8, 200]);
        }
        let dyn_rgb = DynamicImage::ImageRgb8(rgb);
        let proto_rgb =
            Iterm2Protocol::new(&dyn_rgb, unit_font(), Size::new(32, 24), false, true).unwrap();
        let payload_rgb = extract_payload_bytes(proto_rgb.data());
        assert_eq!(
            &payload_rgb[..2],
            &[0xFF, 0xD8],
            "Expected JPEG SOI magic bytes for opaque RGB8 in JPEG mode"
        );
        let decoded = image::load_from_memory(&payload_rgb).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (32, 24));

        // Opaque RGBA8 also uses JPEG when use_jpeg_for_opaque = true
        let mut rgba = RgbaImage::new(32, 24);
        for p in rgba.pixels_mut() {
            *p = Rgba([40, 120, 210, 255]);
        }
        let dyn_rgba = DynamicImage::ImageRgba8(rgba);
        let proto_rgba =
            Iterm2Protocol::new(&dyn_rgba, unit_font(), Size::new(32, 24), false, true).unwrap();
        let payload_rgba = extract_payload_bytes(proto_rgba.data());
        assert_eq!(&payload_rgba[..2], &[0xFF, 0xD8]);
    }

    #[test]
    fn test_iterm2_transparent_rgba8_in_jpeg_mode_falls_back_to_png() {
        let mut rgba = RgbaImage::new(12, 10);
        for (x, y, p) in rgba.enumerate_pixels_mut() {
            let alpha = if (x + y) % 2 == 0 { 255 } else { 64 };
            *p = Rgba([200, 100, 50, alpha]);
        }
        let dyn_rgba = DynamicImage::ImageRgba8(rgba.clone());
        let proto =
            Iterm2Protocol::new(&dyn_rgba, unit_font(), Size::new(12, 10), false, true).unwrap();

        let payload = extract_payload_bytes(proto.data());
        assert_eq!(
            &payload[..8],
            b"\x89PNG\r\n\x1a\n",
            "Transparent image must fall back to PNG even in JPEG mode"
        );
        let decoded = image::load_from_memory(&payload).unwrap().to_rgba8();
        assert_eq!(decoded, rgba, "Alpha channel must be preserved losslessly");
    }

    #[test]
    fn test_iterm2_framing_tmux_clear_area_and_zero_size() {
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 3, Rgb([10, 20, 30])));

        // Zero width or height rejected
        assert!(Iterm2Protocol::new(&img, unit_font(), Size::new(0, 3), false, false).is_err());
        assert!(Iterm2Protocol::new(&img, unit_font(), Size::new(4, 0), false, false).is_err());

        // Single-row clear_area: \x1b[4X
        let single_row = Iterm2Protocol::new(
            &DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 1, Rgb([1, 2, 3]))),
            unit_font(),
            Size::new(4, 1),
            false,
            false,
        )
        .unwrap();
        assert!(
            single_row
                .data()
                .starts_with("\x1b[4X\x1b]1337;File=inline=1;")
        );
        assert!(
            single_row
                .data()
                .contains("width=4px;height=1px;doNotMoveCursor=1:")
        );
        assert!(single_row.data().ends_with('\x07'));

        // Multi-row clear_area: (\x1b[4X\x1b[1B)*3 + \x1b[3A
        let multi_row =
            Iterm2Protocol::new(&img, unit_font(), Size::new(4, 3), false, false).unwrap();
        assert!(multi_row.data().starts_with(
            "\x1b[4X\x1b[1B\x1b[4X\x1b[1B\x1b[4X\x1b[1B\x1b[3A\x1b]1337;File=inline=1;"
        ));

        // Tmux passthrough framing: starts with \x1bPtmux;, doubles ESC, ends with \x07\x1b\\
        let tmux_proto =
            Iterm2Protocol::new(&img, unit_font(), Size::new(4, 3), true, false).unwrap();
        assert!(tmux_proto.data().starts_with("\x1bPtmux;"));
        assert!(tmux_proto.data().contains("\x1b\x1b]1337;File=inline=1;"));
        assert!(tmux_proto.data().ends_with("\x07\x1b\\"));
    }

    #[test]
    fn test_iterm2_render_buffer_invariants() {
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 3, Rgb([10, 20, 30])));
        let proto = Iterm2Protocol::new(&img, unit_font(), Size::new(4, 3), false, false).unwrap();
        assert_eq!(proto.size(), Size::new(4, 3));

        // Rendering into a sufficient area sets top-left to data with UNIT_WIDTH, rest to Skip
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 5));
        proto.render(Rect::new(1, 1, 5, 4), &mut buf);

        let top_left = buf.cell((1, 1)).unwrap();
        assert_eq!(top_left.symbol(), proto.data());
        assert_eq!(top_left.diff_option, UNIT_WIDTH);

        for y in 1..4 {
            for x in 1..5 {
                if x == 1 && y == 1 {
                    continue;
                }
                assert_eq!(
                    buf[(x, y)].diff_option,
                    CellDiffOption::Skip,
                    "Expected cell ({x}, {y}) to be marked Skip"
                );
            }
        }
        // Cell outside rendered area remains untouched
        assert_eq!(buf[(5, 1)].diff_option, CellDiffOption::default());
        assert_eq!(buf[(1, 4)].diff_option, CellDiffOption::default());

        // Rendering into an undersized area is a safe no-op
        let mut small_buf = Buffer::empty(Rect::new(0, 0, 3, 2));
        proto.render(Rect::new(0, 0, 3, 2), &mut small_buf);
        assert_eq!(small_buf[(0, 0)].symbol(), " ");
        assert_eq!(small_buf[(0, 0)].diff_option, CellDiffOption::default());
        assert_eq!(small_buf[(1, 0)].diff_option, CellDiffOption::default());
    }
}
