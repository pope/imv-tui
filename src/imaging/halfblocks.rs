//! High-performance SIMD + `rayon` Unicode halfblocks graphics protocol encoder and renderer.
//!
//! Replaces `ratatui-image`'s single-threaded `image::resize_exact` + `to_rgb8()` path by
//! reusing the worker's SIMD `fast_image_resize::Resizer` (`Bilinear`, `mul_div_alpha = false`)
//! and building a row-major cell grid safe for viewports exceeding `u16::MAX` cells.

use fast_image_resize as fir;
use image::DynamicImage;
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui::style::Color;

/// A single pre-encoded terminal cell representing two vertically stacked pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HalfBlockCell {
    fg: Color,
    bg: Color,
    ch: char,
}

/// Fast SIMD-resized Unicode halfblocks protocol state ready for direct `ratatui` buffer rendering.
pub struct HalfblocksProtocol {
    cells: Vec<HalfBlockCell>,
    size: Size,
}

impl HalfblocksProtocol {
    /// Encodes `canvas` into a `size.width x size.height` grid of Unicode halfblock cells.
    ///
    /// Resizes `canvas` directly to `(size.width, size.height * 2)` using `resizer` with
    /// SIMD `Bilinear` convolution (`mul_div_alpha = false`), skipping resizing completely
    /// when `canvas` already has the exact target pixel dimensions.
    pub fn new(
        canvas: &DynamicImage,
        size: Size,
        resizer: &mut fir::Resizer,
    ) -> Result<Self, String> {
        if size.width == 0 || size.height == 0 {
            return Err("Target size has zero width or height".to_string());
        }
        if canvas.width() == 0 || canvas.height() == 0 {
            return Err("Source image has zero width or height".to_string());
        }

        let cols = usize::from(size.width);
        let rows = usize::from(size.height);
        let dst_w = u32::from(size.width);
        let dst_h = u32::from(size.height) * 2;

        let exact_size = canvas.width() == dst_w && canvas.height() == dst_h;
        let cells = if exact_size {
            match canvas {
                DynamicImage::ImageRgb8(rgb) => encode_rgb_cells(rgb.as_raw(), cols, rows),
                DynamicImage::ImageRgba8(rgba) => encode_rgba_cells(rgba.as_raw(), cols, rows),
                other => {
                    let rgba = super::types::to_rgba8_fast(other);
                    encode_rgba_cells(rgba.as_raw(), cols, rows)
                }
            }
        } else {
            let mut options = fir::ResizeOptions::new();
            options.algorithm = fir::ResizeAlg::Convolution(fir::FilterType::Bilinear);
            options.mul_div_alpha = false;

            match canvas {
                DynamicImage::ImageRgb8(rgb) => {
                    let mut dst = fir::images::Image::new(dst_w, dst_h, fir::PixelType::U8x3);
                    resizer
                        .resize(rgb, &mut dst, Some(&options))
                        .map_err(|e| format!("Halfblocks RGB8 resize failed: {e}"))?;
                    super::types::maybe_reclaim_resizer_scratch(
                        resizer,
                        canvas.width(),
                        canvas.height(),
                        dst_w,
                        dst_h,
                    );
                    encode_rgb_cells(dst.buffer(), cols, rows)
                }
                DynamicImage::ImageRgba8(rgba) => {
                    let mut dst = fir::images::Image::new(dst_w, dst_h, fir::PixelType::U8x4);
                    resizer
                        .resize(rgba, &mut dst, Some(&options))
                        .map_err(|e| format!("Halfblocks RGBA8 resize failed: {e}"))?;
                    super::types::maybe_reclaim_resizer_scratch(
                        resizer,
                        canvas.width(),
                        canvas.height(),
                        dst_w,
                        dst_h,
                    );
                    encode_rgba_cells(dst.buffer(), cols, rows)
                }
                other => {
                    let rgba = super::types::to_rgba8_fast(other);
                    let mut dst = fir::images::Image::new(dst_w, dst_h, fir::PixelType::U8x4);
                    resizer
                        .resize(rgba.as_ref(), &mut dst, Some(&options))
                        .map_err(|e| format!("Halfblocks RGBA8 resize failed: {e}"))?;
                    super::types::maybe_reclaim_resizer_scratch(
                        resizer,
                        canvas.width(),
                        canvas.height(),
                        dst_w,
                        dst_h,
                    );
                    encode_rgba_cells(dst.buffer(), cols, rows)
                }
            }
        };

        Ok(Self { cells, size })
    }

    /// Renders the halfblock cells into `buf` starting at `(area.x, area.y)`, clipping to
    /// `min(self.size, area)` without per-cell division/modulo or `u16` index overflow.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let render_w = self.size.width.min(area.width);
        let render_h = self.size.height.min(area.height);
        if render_w == 0 || render_h == 0 {
            return;
        }
        let stride = usize::from(self.size.width);
        for y in 0..render_h {
            let row_offset = usize::from(y) * stride;
            let dst_y = area.y + y;
            for x in 0..render_w {
                let src = self.cells[row_offset + usize::from(x)];
                if let Some(cell) = buf.cell_mut((area.x + x, dst_y)) {
                    cell.set_char(src.ch).set_fg(src.fg).set_bg(src.bg);
                }
            }
        }
    }

    /// Returns the terminal cell dimensions occupied by this halfblocks image.
    #[cfg(test)]
    pub fn size(&self) -> Size {
        self.size
    }
}

/// Computes Rec. 709 integer luminance (`2126 * R + 7152 * G + 722 * B`) matching `ratatui-image`.
#[inline]
const fn luminance(r: u8, g: u8, b: u8) -> u32 {
    2126 * (r as u32) + 7152 * (g as u32) + 722 * (b as u32)
}

/// Selects the halfblock glyph and foreground/background colors for a vertical pixel pair.
///
/// Uses the exact luminance tie-breaking rules of `ratatui-image`:
/// - Equal pixels: `' '` with `fg = upper`, `bg = lower`
/// - Brighter lower pixel: `'▄'` with `fg = lower`, `bg = upper`
/// - Otherwise: `'▀'` with `fg = upper`, `bg = lower`
#[inline]
const fn pick_side(upper: [u8; 3], lower: [u8; 3]) -> HalfBlockCell {
    let color_upper = Color::Rgb(upper[0], upper[1], upper[2]);
    let color_lower = Color::Rgb(lower[0], lower[1], lower[2]);
    if upper[0] == lower[0] && upper[1] == lower[1] && upper[2] == lower[2] {
        HalfBlockCell {
            fg: color_upper,
            bg: color_lower,
            ch: ' ',
        }
    } else if luminance(lower[0], lower[1], lower[2]) > luminance(upper[0], upper[1], upper[2]) {
        HalfBlockCell {
            fg: color_lower,
            bg: color_upper,
            ch: '▄',
        }
    } else {
        HalfBlockCell {
            fg: color_upper,
            bg: color_lower,
            ch: '▀',
        }
    }
}

/// Encodes a tightly packed `cols x (rows * 2)` RGB8 pixel buffer into `cols * rows` halfblock cells.
fn encode_rgb_cells(raw: &[u8], cols: usize, rows: usize) -> Vec<HalfBlockCell> {
    let mut cells = Vec::with_capacity(cols * rows);
    let row_bytes = cols * 3;
    let pair_bytes = row_bytes * 2;
    for row_pair in raw.chunks_exact(pair_bytes) {
        let (top_row, bot_row) = row_pair.split_at(row_bytes);
        let top_px = top_row.as_chunks::<3>().0;
        let bot_px = bot_row.as_chunks::<3>().0;
        for (&u, &l) in top_px.iter().zip(bot_px.iter()) {
            cells.push(pick_side(u, l));
        }
    }
    cells
}

/// Encodes a tightly packed `cols x (rows * 2)` RGBA8 pixel buffer into `cols * rows` halfblock cells.
fn encode_rgba_cells(raw: &[u8], cols: usize, rows: usize) -> Vec<HalfBlockCell> {
    let mut cells = Vec::with_capacity(cols * rows);
    let row_bytes = cols * 4;
    let pair_bytes = row_bytes * 2;
    for row_pair in raw.chunks_exact(pair_bytes) {
        let (top_row, bot_row) = row_pair.split_at(row_bytes);
        let top_px = top_row.as_chunks::<4>().0;
        let bot_px = bot_row.as_chunks::<4>().0;
        for (&[ur, ug, ub, _], &[lr, lg, lb, _]) in top_px.iter().zip(bot_px.iter()) {
            cells.push(pick_side([ur, ug, ub], [lr, lg, lb]));
        }
    }
    cells
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GrayImage, Luma, Rgb, RgbImage, Rgba, RgbaImage};

    #[test]
    fn test_pick_side_luminance_tie_breaking() {
        // Equal colors -> space glyph
        let eq = pick_side([120, 80, 40], [120, 80, 40]);
        assert_eq!(eq.ch, ' ');
        assert_eq!(eq.fg, Color::Rgb(120, 80, 40));
        assert_eq!(eq.bg, Color::Rgb(120, 80, 40));

        // Brighter upper pixel -> upper half block '▀'
        let upper_bright = pick_side([240, 240, 240], [10, 20, 30]);
        assert_eq!(upper_bright.ch, '▀');
        assert_eq!(upper_bright.fg, Color::Rgb(240, 240, 240));
        assert_eq!(upper_bright.bg, Color::Rgb(10, 20, 30));

        // Brighter lower pixel -> lower half block '▄'
        let lower_bright = pick_side([10, 20, 30], [200, 250, 100]);
        assert_eq!(lower_bright.ch, '▄');
        assert_eq!(lower_bright.fg, Color::Rgb(200, 250, 100));
        assert_eq!(lower_bright.bg, Color::Rgb(10, 20, 30));

        // Equal luminance (71,520) but distinct RGB -> upper half block '▀'
        // (lower is NOT strictly brighter)
        let tie_upper = [17, 0, 49];
        let tie_lower = [0, 10, 0];
        assert_eq!(
            luminance(tie_upper[0], tie_upper[1], tie_upper[2]),
            luminance(tie_lower[0], tie_lower[1], tie_lower[2])
        );
        let tie = pick_side(tie_upper, tie_lower);
        assert_eq!(tie.ch, '▀');
        assert_eq!(tie.fg, Color::Rgb(17, 0, 49));
        assert_eq!(tie.bg, Color::Rgb(0, 10, 0));
    }

    #[test]
    fn test_rgb8_and_rgba8_exact_size_skip_resize_and_render() {
        let mut resizer = fir::Resizer::new();
        let size = Size::new(2, 2); // 2x4 pixels

        // RGB8 exact-size image
        let mut rgb = RgbImage::new(2, 4);
        // Cell (0, 0): upper bright white, lower dark -> '▀'
        rgb.put_pixel(0, 0, Rgb([255, 255, 255]));
        rgb.put_pixel(0, 1, Rgb([10, 10, 10]));
        // Cell (1, 0): equal -> ' '
        rgb.put_pixel(1, 0, Rgb([42, 43, 44]));
        rgb.put_pixel(1, 1, Rgb([42, 43, 44]));
        // Cell (0, 1): lower bright -> '▄'
        rgb.put_pixel(0, 2, Rgb([5, 5, 5]));
        rgb.put_pixel(0, 3, Rgb([200, 100, 50]));
        // Cell (1, 1): upper bright green -> '▀'
        rgb.put_pixel(1, 2, Rgb([0, 255, 0]));
        rgb.put_pixel(1, 3, Rgb([0, 0, 255]));

        let proto_rgb =
            HalfblocksProtocol::new(&DynamicImage::ImageRgb8(rgb), size, &mut resizer).unwrap();
        assert_eq!(proto_rgb.size(), size);

        let mut buf = Buffer::empty(Rect::new(0, 0, 2, 2));
        proto_rgb.render(Rect::new(0, 0, 2, 2), &mut buf);

        assert_eq!(buf.cell((0, 0)).unwrap().symbol(), "▀");
        assert_eq!(buf.cell((0, 0)).unwrap().fg, Color::Rgb(255, 255, 255));
        assert_eq!(buf.cell((0, 0)).unwrap().bg, Color::Rgb(10, 10, 10));

        assert_eq!(buf.cell((1, 0)).unwrap().symbol(), " ");
        assert_eq!(buf.cell((1, 0)).unwrap().fg, Color::Rgb(42, 43, 44));
        assert_eq!(buf.cell((1, 0)).unwrap().bg, Color::Rgb(42, 43, 44));

        assert_eq!(buf.cell((0, 1)).unwrap().symbol(), "▄");
        assert_eq!(buf.cell((0, 1)).unwrap().fg, Color::Rgb(200, 100, 50));
        assert_eq!(buf.cell((0, 1)).unwrap().bg, Color::Rgb(5, 5, 5));

        assert_eq!(buf.cell((1, 1)).unwrap().symbol(), "▀");
        assert_eq!(buf.cell((1, 1)).unwrap().fg, Color::Rgb(0, 255, 0));
        assert_eq!(buf.cell((1, 1)).unwrap().bg, Color::Rgb(0, 0, 255));

        // RGBA8 exact-size image (alpha ignored, matching ratatui-image's to_rgb8())
        let mut rgba = RgbaImage::new(2, 4);
        rgba.put_pixel(0, 0, Rgba([255, 255, 255, 0]));
        rgba.put_pixel(0, 1, Rgba([10, 10, 10, 128]));
        rgba.put_pixel(1, 0, Rgba([42, 43, 44, 255]));
        rgba.put_pixel(1, 1, Rgba([42, 43, 44, 0]));
        rgba.put_pixel(0, 2, Rgba([5, 5, 5, 255]));
        rgba.put_pixel(0, 3, Rgba([200, 100, 50, 64]));
        rgba.put_pixel(1, 2, Rgba([0, 255, 0, 255]));
        rgba.put_pixel(1, 3, Rgba([0, 0, 255, 255]));

        let proto_rgba =
            HalfblocksProtocol::new(&DynamicImage::ImageRgba8(rgba), size, &mut resizer).unwrap();
        assert_eq!(proto_rgba.cells, proto_rgb.cells);
    }

    #[test]
    fn test_downscale_path_rgb8_rgba8_and_luma8() {
        let mut resizer = fir::Resizer::new();
        let size = Size::new(8, 4); // 8x8 pixels

        // Solid top half red, solid bottom half white on 64x64 image
        let mut rgb = RgbImage::new(64, 64);
        for y in 0..64 {
            let color = if y < 32 {
                Rgb([200, 20, 20])
            } else {
                Rgb([240, 240, 240])
            };
            for x in 0..64 {
                rgb.put_pixel(x, y, color);
            }
        }
        let proto_rgb =
            HalfblocksProtocol::new(&DynamicImage::ImageRgb8(rgb), size, &mut resizer).unwrap();
        assert_eq!(proto_rgb.cells.len(), 8 * 4);
        // Row 0 is solidly red -> space glyph
        assert_eq!(proto_rgb.cells[0].ch, ' ');
        assert_eq!(proto_rgb.cells[0].fg, Color::Rgb(200, 20, 20));
        // Row 3 is solidly white -> space glyph
        assert_eq!(proto_rgb.cells[3 * 8].ch, ' ');
        assert_eq!(proto_rgb.cells[3 * 8].fg, Color::Rgb(240, 240, 240));

        // RGBA8 downscale path (with alpha=0 to verify mul_div_alpha=false preserves RGB)
        let rgba = RgbaImage::from_pixel(32, 32, Rgba([100, 150, 200, 0]));
        let proto_rgba =
            HalfblocksProtocol::new(&DynamicImage::ImageRgba8(rgba), size, &mut resizer).unwrap();
        assert_eq!(proto_rgba.cells.len(), 8 * 4);
        assert_eq!(proto_rgba.cells[0].ch, ' ');
        assert_eq!(proto_rgba.cells[0].fg, Color::Rgb(100, 150, 200));

        // Non-RGB8/RGBA8 fallback variant (Luma8)
        let luma = GrayImage::from_pixel(32, 32, Luma([180]));
        let proto_luma =
            HalfblocksProtocol::new(&DynamicImage::ImageLuma8(luma), size, &mut resizer).unwrap();
        assert_eq!(proto_luma.cells.len(), 8 * 4);
        assert_eq!(proto_luma.cells[0].ch, ' ');
        assert_eq!(proto_luma.cells[0].fg, Color::Rgb(180, 180, 180));
    }

    #[test]
    fn test_large_grid_exceeding_u16_max_cells() {
        let mut resizer = fir::Resizer::new();
        // 458 * 144 = 65,952 > u16::MAX (65,535)
        let size = Size::new(458, 144);
        let dst_w = u32::from(size.width);
        let dst_h = u32::from(size.height) * 2;

        let mut img = RgbImage::from_pixel(dst_w, dst_h, Rgb([30, 30, 30]));
        // Distinct top-left cell (0, 0): bright red over black -> '▀'
        img.put_pixel(0, 0, Rgb([250, 10, 10]));
        img.put_pixel(0, 1, Rgb([0, 0, 0]));
        // Distinct bottom-right cell (457, 143): black over bright cyan -> '▄'
        img.put_pixel(dst_w - 1, dst_h - 2, Rgb([0, 0, 0]));
        img.put_pixel(dst_w - 1, dst_h - 1, Rgb([10, 240, 250]));

        let proto =
            HalfblocksProtocol::new(&DynamicImage::ImageRgb8(img), size, &mut resizer).unwrap();
        assert_eq!(proto.cells.len(), 458 * 144);

        let area = Rect::new(0, 0, size.width, size.height);
        let mut buf = Buffer::empty(area);
        proto.render(area, &mut buf);

        // Top-left cell must NOT be overwritten by wrapped bottom rows
        let top_left = buf.cell((0, 0)).unwrap();
        assert_eq!(top_left.symbol(), "▀");
        assert_eq!(top_left.fg, Color::Rgb(250, 10, 10));
        assert_eq!(top_left.bg, Color::Rgb(0, 0, 0));

        // Bottom-right cell must be rendered at exact (457, 143)
        let bot_right = buf.cell((457, 143)).unwrap();
        assert_eq!(bot_right.symbol(), "▄");
        assert_eq!(bot_right.fg, Color::Rgb(10, 240, 250));
        assert_eq!(bot_right.bg, Color::Rgb(0, 0, 0));
    }

    #[test]
    fn test_zero_size_error_and_clipped_render() {
        let mut resizer = fir::Resizer::new();
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(16, 16, Rgb([10, 20, 30])));

        assert!(HalfblocksProtocol::new(&img, Size::new(0, 10), &mut resizer).is_err());
        assert!(HalfblocksProtocol::new(&img, Size::new(10, 0), &mut resizer).is_err());
        assert!(HalfblocksProtocol::new(&img, Size::new(0, 0), &mut resizer).is_err());

        let zero_img = DynamicImage::ImageRgb8(RgbImage::new(0, 0));
        assert!(HalfblocksProtocol::new(&zero_img, Size::new(4, 4), &mut resizer).is_err());

        // Clipped render when area is smaller than self.size and offset from origin
        let mut src = RgbImage::from_pixel(4, 8, Rgb([50, 60, 70]));
        src.put_pixel(1, 2, Rgb([255, 0, 0])); // cell (1, 1) upper
        src.put_pixel(1, 3, Rgb([0, 0, 0])); // cell (1, 1) lower
        let proto =
            HalfblocksProtocol::new(&DynamicImage::ImageRgb8(src), Size::new(4, 4), &mut resizer)
                .unwrap();

        let mut buf = Buffer::empty(Rect::new(0, 0, 5, 5));
        // Render into a 2x2 area at offset (1, 1)
        proto.render(Rect::new(1, 1, 2, 2), &mut buf);

        // Cell (1, 1) of protocol maps to buffer (2, 2)
        let mapped = buf.cell((2, 2)).unwrap();
        assert_eq!(mapped.symbol(), "▀");
        assert_eq!(mapped.fg, Color::Rgb(255, 0, 0));
        assert_eq!(mapped.bg, Color::Rgb(0, 0, 0));

        // Outside the clipped area (3, 1) remains untouched default
        assert_eq!(buf.cell((3, 1)).unwrap().fg, Color::Reset);
        assert_eq!(buf.cell((0, 0)).unwrap().fg, Color::Reset);

        // Zero-area render is a safe no-op
        proto.render(Rect::new(1, 1, 0, 2), &mut buf);
        proto.render(Rect::new(1, 1, 2, 0), &mut buf);
    }
}
