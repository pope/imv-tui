//! Unified viewport graphics protocol abstraction.
//!
//! Dispatches rendering across zero-copy POSIX shared memory Kitty (`t=s`),
//! parallel `rayon` inline Kitty (`t=d` / `o=z,t=d`), parallel `rayon` Sixel,
//! fast PNG/JPEG iTerm2 (`OSC 1337`), and SIMD Unicode halfblocks.

use std::borrow::Cow;

use image::{DynamicImage, RgbImage, RgbaImage};
use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};

use super::halfblocks::HalfblocksProtocol;
use super::iterm2::Iterm2Protocol;
use super::kitty_direct::KittyDirectProtocol;
use super::kitty_shm::KittyShmProtocol;
use super::picker::FontSize;
use super::sixel::SixelProtocol;

/// Copies the top-left `dst_w x dst_h` region from a tightly packed `src_w`-wide pixel buffer
/// (`BPP` bytes per pixel) without zero-initializing the destination buffer.
fn crop_top_left_bytes<const BPP: usize>(
    src: &[u8],
    src_w: u32,
    dst_w: u32,
    dst_h: u32,
) -> Vec<u8> {
    let src_stride = src_w as usize * BPP;
    let row_bytes = dst_w as usize * BPP;
    let dst_h = dst_h as usize;
    let total_bytes = row_bytes * dst_h;
    let mut dst = Vec::with_capacity(total_bytes);
    if total_bytes == 0 {
        return dst;
    }
    if src_stride == row_bytes {
        dst.extend_from_slice(&src[..total_bytes]);
    } else {
        for y in 0..dst_h {
            let src_start = y * src_stride;
            dst.extend_from_slice(&src[src_start..src_start + row_bytes]);
        }
    }
    dst
}

/// Copies the top-left `copy_w x copy_h` RGBA8 pixels from `src` into `dst` (with row stride
/// `dst_stride` bytes) using direct row-slice copies.
fn copy_rgba_rows(
    src: &RgbaImage,
    dst: &mut [u8],
    dst_stride: usize,
    copy_w: usize,
    copy_h: usize,
) {
    let src_raw = src.as_raw();
    let src_stride = src.width() as usize * 4;
    let row_bytes = copy_w * 4;
    if src_stride == row_bytes && dst_stride == row_bytes {
        let total_bytes = row_bytes * copy_h;
        dst[..total_bytes].copy_from_slice(&src_raw[..total_bytes]);
    } else {
        for y in 0..copy_h {
            let src_start = y * src_stride;
            let dst_start = y * dst_stride;
            dst[dst_start..dst_start + row_bytes]
                .copy_from_slice(&src_raw[src_start..src_start + row_bytes]);
        }
    }
}

/// Normalizes a rendered pixel canvas to the exact pixel dimensions of `size * font_size`.
///
/// - If `canvas` already matches `target_px_w x target_px_h`, returns `Cow::Borrowed(canvas)`
///   with zero allocation or copy.
/// - If `canvas` is at least as large on both axes, crops the top-left `target_px_w x target_px_h`
///   region without re-encoding or changing color format.
/// - If `canvas` is smaller on either axis, crops top-left to `min(w, target_px_w) x min(h, target_px_h)`
///   and pads onto a transparent `target_px_w x target_px_h` RGBA8 canvas so terminal cell-grid
///   protocols (such as Kitty Unicode placeholders) never distort aspect ratio.
pub fn normalize_canvas_to_cells<'a>(
    canvas: &'a DynamicImage,
    font_size: FontSize,
    size: Size,
) -> Cow<'a, DynamicImage> {
    let target_px_w = u32::from(size.width) * u32::from(font_size.width);
    let target_px_h = u32::from(size.height) * u32::from(font_size.height);

    if canvas.width() == target_px_w && canvas.height() == target_px_h {
        return Cow::Borrowed(canvas);
    }

    if canvas.width() >= target_px_w && canvas.height() >= target_px_h {
        return match canvas {
            DynamicImage::ImageRgb8(rgb) => {
                let dst =
                    crop_top_left_bytes::<3>(rgb.as_raw(), rgb.width(), target_px_w, target_px_h);
                Cow::Owned(DynamicImage::ImageRgb8(
                    RgbImage::from_raw(target_px_w, target_px_h, dst)
                        .expect("valid RGB crop dimensions"),
                ))
            }
            DynamicImage::ImageRgba8(rgba) => {
                let dst =
                    crop_top_left_bytes::<4>(rgba.as_raw(), rgba.width(), target_px_w, target_px_h);
                Cow::Owned(DynamicImage::ImageRgba8(
                    RgbaImage::from_raw(target_px_w, target_px_h, dst)
                        .expect("valid RGBA crop dimensions"),
                ))
            }
            other => Cow::Owned(other.crop_imm(0, 0, target_px_w, target_px_h)),
        };
    }

    let copy_w = canvas.width().min(target_px_w) as usize;
    let copy_h = canvas.height().min(target_px_h) as usize;
    let mut padded = RgbaImage::new(target_px_w, target_px_h);

    if copy_w > 0 && copy_h > 0 {
        let dst: &mut [u8] = padded.as_mut();
        let dst_stride = target_px_w as usize * 4;
        match canvas {
            DynamicImage::ImageRgba8(rgba) => {
                copy_rgba_rows(rgba, dst, dst_stride, copy_w, copy_h);
            }
            DynamicImage::ImageRgb8(rgb) => {
                let src = rgb.as_raw();
                let src_stride = rgb.width() as usize * 3;
                let src_row_bytes = copy_w * 3;
                let dst_row_bytes = copy_w * 4;
                if src_stride == src_row_bytes && dst_stride == dst_row_bytes {
                    super::types::unpack_rgb24_into_rgba32(
                        &src[..src_row_bytes * copy_h],
                        &mut dst[..dst_row_bytes * copy_h],
                    );
                } else {
                    for y in 0..copy_h {
                        let src_start = y * src_stride;
                        let dst_start = y * dst_stride;
                        super::types::unpack_rgb24_into_rgba32(
                            &src[src_start..src_start + src_row_bytes],
                            &mut dst[dst_start..dst_start + dst_row_bytes],
                        );
                    }
                }
            }
            other => {
                let rgba = super::types::to_rgba8_fast(other);
                copy_rgba_rows(&rgba, dst, dst_stride, copy_w, copy_h);
            }
        }
    }

    Cow::Owned(DynamicImage::ImageRgba8(padded))
}

/// Transmission mode for the Kitty graphics protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KittyTransferMode {
    /// Standard uncompressed inline Base64 transmission (`t=d`).
    #[default]
    Direct,
    /// Zlib-compressed inline Base64 transmission (`o=z,t=d`).
    Compressed,
    /// Zero-copy POSIX shared memory transmission (`t=s`).
    SharedMemory,
}

/// Transmission encoding mode for the iTerm2 inline image protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Iterm2TransferMode {
    /// Lossless PNG (`Fast` + `Paeth`) inline transmission.
    #[default]
    Png,
    /// Quality-90 JPEG inline transmission for opaque images (falls back to PNG when transparent).
    Jpeg,
}

/// Palette compression mode for the DEC Sixel graphics protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SixelTransferMode {
    /// Full 256-color palette with 4x4 Bayer ordered dithering.
    #[default]
    Full,
    /// Reduced 128-color palette with 4x4 Bayer ordered dithering for smaller payloads.
    Compressed,
}

/// Active runtime terminal graphics protocol and its protocol-specific configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActiveProtocol {
    /// Kitty graphics protocol with its transmission mode.
    Kitty(KittyTransferMode),
    /// DEC Sixel graphics protocol with its palette compression mode.
    Sixel(SixelTransferMode),
    /// iTerm2 inline images protocol with its encoding mode.
    Iterm2(Iterm2TransferMode),
    /// Unicode halfblock character fallback.
    #[default]
    Halfblocks,
}

impl ActiveProtocol {
    #[cfg(test)]
    /// Constructs an `ActiveProtocol` from a `ProtocolType` and a compression state.
    pub fn from_protocol_type(proto: super::picker::ProtocolType, compressed: bool) -> Self {
        match proto {
            super::picker::ProtocolType::Kitty => {
                if compressed {
                    Self::Kitty(KittyTransferMode::Compressed)
                } else {
                    Self::Kitty(KittyTransferMode::Direct)
                }
            }
            super::picker::ProtocolType::Sixel => {
                if compressed {
                    Self::Sixel(SixelTransferMode::Compressed)
                } else {
                    Self::Sixel(SixelTransferMode::Full)
                }
            }
            super::picker::ProtocolType::Iterm2 => {
                if compressed {
                    Self::Iterm2(Iterm2TransferMode::Jpeg)
                } else {
                    Self::Iterm2(Iterm2TransferMode::Png)
                }
            }
            super::picker::ProtocolType::Halfblocks => Self::Halfblocks,
        }
    }

    /// Returns an updated `ActiveProtocol` enabling or disabling Kitty shared memory transmission.
    ///
    /// Only upgrades `Self::Kitty(KittyTransferMode::Direct)` to `Self::Kitty(KittyTransferMode::SharedMemory)`
    /// when `shm_supported` is `true` (never touching `Compressed`).
    pub const fn with_kitty_shm(self, shm_supported: bool) -> Self {
        match self {
            Self::Kitty(KittyTransferMode::Direct) if shm_supported => {
                Self::Kitty(KittyTransferMode::SharedMemory)
            }
            Self::Kitty(KittyTransferMode::SharedMemory) if !shm_supported => {
                Self::Kitty(KittyTransferMode::Direct)
            }
            other => other,
        }
    }

    /// Returns the descriptive label for Stats for Nerds, indicating compression or SHM only when active.
    pub const fn display_label(self) -> &'static str {
        match self {
            Self::Kitty(KittyTransferMode::Direct) => "Kitty",
            Self::Kitty(KittyTransferMode::Compressed) => "Kitty (Compressed)",
            Self::Kitty(KittyTransferMode::SharedMemory) => "Kitty (SHM)",
            Self::Sixel(SixelTransferMode::Full) => "Sixel",
            Self::Sixel(SixelTransferMode::Compressed) => "Sixel (Compressed)",
            Self::Iterm2(Iterm2TransferMode::Png) => "Iterm2",
            Self::Iterm2(Iterm2TransferMode::Jpeg) => "Iterm2 (JPEG)",
            Self::Halfblocks => "Halfblocks",
        }
    }
}

/// Unified viewport image protocol supporting zero-copy POSIX shared memory (`t=s`) Kitty,
/// parallel inline (`t=d` / `o=z,t=d`) Kitty, parallel Sixel, fast PNG/JPEG iTerm2 (`OSC 1337`),
/// SIMD Unicode halfblocks, and a no-op empty fallback for zero-size or failed encodes.
pub enum ViewportProtocol {
    /// No-op fallback used when target dimensions are zero or protocol encoding fails.
    Empty,
    /// Zero-copy POSIX shared memory Kitty protocol (`t=s`).
    KittyShm(KittyShmProtocol),
    /// Parallel `rayon`-accelerated inline Kitty protocol (`t=d` / `o=z,t=d`).
    KittyDirect(KittyDirectProtocol),
    /// Parallel `rayon`-accelerated Sixel protocol.
    Sixel(SixelProtocol),
    /// Fast PNG (`Fast` + `Paeth`) / JPEG (q90) iTerm2 inline image protocol (`OSC 1337`).
    Iterm2(Iterm2Protocol),
    /// Fast SIMD-resized Unicode halfblocks protocol.
    Halfblocks(HalfblocksProtocol),
}

impl ViewportProtocol {
    /// Writes any pending one-shot protocol transmission escape sequence directly to `writer`.
    ///
    /// Currently used by [`Self::KittyDirect`] to bypass copying large Base64 payloads into
    /// ratatui's cell buffer. Returns `Ok(true)` if bytes were written, or `Ok(false)` if
    /// nothing was pending or the protocol does not use direct pre-draw transmission.
    pub fn write_pending_transmit(&self, writer: impl std::io::Write) -> std::io::Result<bool> {
        match self {
            Self::KittyDirect(proto) => proto.write_pending_transmit(writer),
            Self::Empty
            | Self::KittyShm(_)
            | Self::Sixel(_)
            | Self::Iterm2(_)
            | Self::Halfblocks(_) => Ok(false),
        }
    }

    /// Renders the encoded viewport image into `buf` within `area`.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        match self {
            Self::Empty => {}
            Self::KittyShm(proto) => proto.render(area, buf),
            Self::KittyDirect(proto) => proto.render(area, buf),
            Self::Sixel(proto) => proto.render(area, buf),
            Self::Iterm2(proto) => proto.render(area, buf),
            Self::Halfblocks(proto) => proto.render(area, buf),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{GrayImage, Luma, Rgb, Rgba};

    #[test]
    fn test_normalize_canvas_exact_borrows() {
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(16, 32, Rgb([10, 20, 30])));
        let out = normalize_canvas_to_cells(&img, FontSize::new(8, 16), Size::new(2, 2));
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!((out.width(), out.height()), (16, 32));
    }

    #[test]
    fn test_normalize_canvas_larger_crops_top_left_rgb8() {
        let mut rgb = RgbImage::from_pixel(20, 40, Rgb([1, 2, 3]));
        rgb.put_pixel(0, 0, Rgb([200, 100, 50]));
        rgb.put_pixel(15, 31, Rgb([40, 50, 60]));
        rgb.put_pixel(16, 0, Rgb([255, 0, 0]));
        rgb.put_pixel(0, 32, Rgb([0, 255, 0]));
        let img = DynamicImage::ImageRgb8(rgb);

        let out = normalize_canvas_to_cells(&img, FontSize::new(8, 16), Size::new(2, 2));
        assert!(matches!(out, Cow::Owned(_)));
        assert_eq!((out.width(), out.height()), (16, 32));
        let cropped = out.as_rgb8().expect("crop preserves RGB8 format");
        assert_eq!(*cropped.get_pixel(0, 0), Rgb([200, 100, 50]));
        assert_eq!(*cropped.get_pixel(15, 31), Rgb([40, 50, 60]));
        assert_eq!(*cropped.get_pixel(1, 0), Rgb([1, 2, 3]));
    }

    #[test]
    fn test_normalize_canvas_larger_crops_top_left_rgba8() {
        let mut rgba = RgbaImage::from_pixel(20, 40, Rgba([7, 8, 9, 200]));
        rgba.put_pixel(0, 0, Rgba([210, 110, 60, 128]));
        rgba.put_pixel(15, 31, Rgba([41, 51, 61, 33]));
        rgba.put_pixel(16, 0, Rgba([255, 0, 0, 255]));
        let img = DynamicImage::ImageRgba8(rgba);

        let out = normalize_canvas_to_cells(&img, FontSize::new(8, 16), Size::new(2, 2));
        assert!(matches!(out, Cow::Owned(_)));
        assert_eq!((out.width(), out.height()), (16, 32));
        let cropped = out.as_rgba8().expect("crop preserves RGBA8 format");
        assert_eq!(*cropped.get_pixel(0, 0), Rgba([210, 110, 60, 128]));
        assert_eq!(*cropped.get_pixel(15, 31), Rgba([41, 51, 61, 33]));
        assert_eq!(*cropped.get_pixel(1, 0), Rgba([7, 8, 9, 200]));
    }

    #[test]
    fn test_normalize_canvas_same_width_height_crop_and_pad() {
        // Same width (16 == 16), taller height (40 > 32) -> contiguous crop.
        let mut rgb_tall = RgbImage::from_pixel(16, 40, Rgb([11, 22, 33]));
        rgb_tall.put_pixel(15, 31, Rgb([99, 88, 77]));
        let img_tall = DynamicImage::ImageRgb8(rgb_tall);
        let out_crop = normalize_canvas_to_cells(&img_tall, FontSize::new(8, 16), Size::new(2, 2));
        let cropped = out_crop.as_rgb8().expect("crop preserves RGB8 format");
        assert_eq!((cropped.width(), cropped.height()), (16, 32));
        assert_eq!(*cropped.get_pixel(15, 31), Rgb([99, 88, 77]));

        // Same width (16 == 16), shorter height (10 < 32) -> contiguous unpack + bottom pad.
        let mut rgb_short = RgbImage::from_pixel(16, 10, Rgb([12, 34, 56]));
        rgb_short.put_pixel(15, 9, Rgb([201, 102, 53]));
        let img_short = DynamicImage::ImageRgb8(rgb_short);
        let out_pad = normalize_canvas_to_cells(&img_short, FontSize::new(8, 16), Size::new(2, 2));
        let padded = out_pad.as_rgba8().expect("padded canvas is RGBA8");
        assert_eq!((padded.width(), padded.height()), (16, 32));
        assert_eq!(*padded.get_pixel(0, 0), Rgba([12, 34, 56, 255]));
        assert_eq!(*padded.get_pixel(15, 9), Rgba([201, 102, 53, 255]));
        assert_eq!(*padded.get_pixel(0, 10), Rgba([0, 0, 0, 0]));
        assert_eq!(*padded.get_pixel(15, 31), Rgba([0, 0, 0, 0]));
    }

    #[test]
    fn test_normalize_canvas_smaller_pads_transparent() {
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(10, 12, Rgb([12, 34, 56])));
        let out = normalize_canvas_to_cells(&img, FontSize::new(8, 16), Size::new(2, 2));
        assert!(matches!(out, Cow::Owned(_)));
        assert_eq!((out.width(), out.height()), (16, 32));
        let rgba = out.as_rgba8().expect("padded canvas is RGBA8");
        assert_eq!(*rgba.get_pixel(0, 0), Rgba([12, 34, 56, 255]));
        assert_eq!(*rgba.get_pixel(9, 11), Rgba([12, 34, 56, 255]));
        assert_eq!(*rgba.get_pixel(10, 11), Rgba([0, 0, 0, 0]));
        assert_eq!(*rgba.get_pixel(15, 31), Rgba([0, 0, 0, 0]));
    }

    #[test]
    fn test_normalize_canvas_mixed_crop_and_pad_rgb8() {
        // Wider than target (20 > 16) and shorter than target (10 < 32).
        let mut rgb = RgbImage::from_pixel(20, 10, Rgb([5, 10, 15]));
        rgb.put_pixel(15, 9, Rgb([200, 150, 100]));
        rgb.put_pixel(19, 9, Rgb([255, 0, 0])); // Outside target width; must be cropped away.
        let img = DynamicImage::ImageRgb8(rgb);

        let out = normalize_canvas_to_cells(&img, FontSize::new(8, 16), Size::new(2, 2));
        assert!(matches!(out, Cow::Owned(_)));
        assert_eq!((out.width(), out.height()), (16, 32));
        let rgba = out.as_rgba8().expect("padded canvas is RGBA8");
        assert_eq!(*rgba.get_pixel(0, 0), Rgba([5, 10, 15, 255]));
        assert_eq!(*rgba.get_pixel(15, 9), Rgba([200, 150, 100, 255]));
        assert_eq!(*rgba.get_pixel(0, 10), Rgba([0, 0, 0, 0]));
        assert_eq!(*rgba.get_pixel(15, 31), Rgba([0, 0, 0, 0]));

        // Narrower than target (10 < 16) and taller than target (40 > 32).
        let mut rgb_narrow = RgbImage::from_pixel(10, 40, Rgb([7, 14, 21]));
        rgb_narrow.put_pixel(9, 31, Rgb([180, 120, 60]));
        rgb_narrow.put_pixel(9, 32, Rgb([255, 0, 0])); // Outside target height; must be cropped away.
        let img_narrow = DynamicImage::ImageRgb8(rgb_narrow);
        let out_narrow =
            normalize_canvas_to_cells(&img_narrow, FontSize::new(8, 16), Size::new(2, 2));
        assert!(matches!(out_narrow, Cow::Owned(_)));
        assert_eq!((out_narrow.width(), out_narrow.height()), (16, 32));
        let rgba_narrow = out_narrow.as_rgba8().expect("padded canvas is RGBA8");
        assert_eq!(*rgba_narrow.get_pixel(0, 0), Rgba([7, 14, 21, 255]));
        assert_eq!(*rgba_narrow.get_pixel(9, 31), Rgba([180, 120, 60, 255]));
        assert_eq!(*rgba_narrow.get_pixel(10, 0), Rgba([0, 0, 0, 0]));
        assert_eq!(*rgba_narrow.get_pixel(15, 31), Rgba([0, 0, 0, 0]));
    }

    #[test]
    fn test_normalize_canvas_mixed_crop_and_pad_rgba8() {
        // Wider than target (20 > 16) and shorter than target (10 < 32).
        let mut wide = RgbaImage::from_pixel(20, 10, Rgba([5, 10, 15, 180]));
        wide.put_pixel(15, 9, Rgba([200, 150, 100, 77]));
        wide.put_pixel(19, 9, Rgba([255, 0, 0, 255]));
        let img_wide = DynamicImage::ImageRgba8(wide);
        let out_wide = normalize_canvas_to_cells(&img_wide, FontSize::new(8, 16), Size::new(2, 2));
        let rgba_wide = out_wide.as_rgba8().expect("padded canvas is RGBA8");
        assert_eq!((rgba_wide.width(), rgba_wide.height()), (16, 32));
        assert_eq!(*rgba_wide.get_pixel(0, 0), Rgba([5, 10, 15, 180]));
        assert_eq!(*rgba_wide.get_pixel(15, 9), Rgba([200, 150, 100, 77]));
        assert_eq!(*rgba_wide.get_pixel(0, 10), Rgba([0, 0, 0, 0]));
        assert_eq!(*rgba_wide.get_pixel(15, 31), Rgba([0, 0, 0, 0]));

        // Narrower than target (10 < 16) and taller than target (40 > 32).
        let mut narrow = RgbaImage::from_pixel(10, 40, Rgba([9, 18, 27, 90]));
        narrow.put_pixel(9, 31, Rgba([111, 222, 33, 44]));
        narrow.put_pixel(9, 35, Rgba([255, 0, 0, 255]));
        let img_narrow = DynamicImage::ImageRgba8(narrow);
        let out_narrow =
            normalize_canvas_to_cells(&img_narrow, FontSize::new(8, 16), Size::new(2, 2));
        let rgba_narrow = out_narrow.as_rgba8().expect("padded canvas is RGBA8");
        assert_eq!((rgba_narrow.width(), rgba_narrow.height()), (16, 32));
        assert_eq!(*rgba_narrow.get_pixel(0, 0), Rgba([9, 18, 27, 90]));
        assert_eq!(*rgba_narrow.get_pixel(9, 31), Rgba([111, 222, 33, 44]));
        assert_eq!(*rgba_narrow.get_pixel(10, 0), Rgba([0, 0, 0, 0]));
        assert_eq!(*rgba_narrow.get_pixel(15, 31), Rgba([0, 0, 0, 0]));
    }

    #[test]
    fn test_normalize_canvas_zero_target_dimensions() {
        let rgb = DynamicImage::ImageRgb8(RgbImage::from_pixel(16, 32, Rgb([1, 2, 3])));
        let out_w0 = normalize_canvas_to_cells(&rgb, FontSize::new(8, 16), Size::new(0, 2));
        assert!(matches!(out_w0, Cow::Owned(_)));
        assert_eq!((out_w0.width(), out_w0.height()), (0, 32));
        assert!(out_w0.as_rgb8().is_some());

        let out_h0 = normalize_canvas_to_cells(&rgb, FontSize::new(8, 16), Size::new(2, 0));
        assert!(matches!(out_h0, Cow::Owned(_)));
        assert_eq!((out_h0.width(), out_h0.height()), (16, 0));
        assert!(out_h0.as_rgb8().is_some());

        let rgba = DynamicImage::ImageRgba8(RgbaImage::from_pixel(16, 32, Rgba([1, 2, 3, 4])));
        let out_both0 = normalize_canvas_to_cells(&rgba, FontSize::new(8, 16), Size::new(0, 0));
        assert!(matches!(out_both0, Cow::Owned(_)));
        assert_eq!((out_both0.width(), out_both0.height()), (0, 0));
        assert!(out_both0.as_rgba8().is_some());
    }

    #[test]
    fn test_normalize_canvas_other_variant_fallback() {
        let luma = DynamicImage::ImageLuma8(GrayImage::from_pixel(20, 40, Luma([77])));
        let cropped = normalize_canvas_to_cells(&luma, FontSize::new(8, 16), Size::new(2, 2));
        assert_eq!((cropped.width(), cropped.height()), (16, 32));
        assert_eq!(
            *cropped
                .as_luma8()
                .expect("crop preserves Luma8")
                .get_pixel(0, 0),
            Luma([77])
        );

        let small_luma = DynamicImage::ImageLuma8(GrayImage::from_pixel(6, 6, Luma([99])));
        let padded = normalize_canvas_to_cells(&small_luma, FontSize::new(8, 16), Size::new(1, 1));
        let rgba = padded.as_rgba8().expect("padded canvas is RGBA8");
        assert_eq!((rgba.width(), rgba.height()), (8, 16));
        assert_eq!(*rgba.get_pixel(5, 5), Rgba([99, 99, 99, 255]));
        assert_eq!(*rgba.get_pixel(6, 5), Rgba([0, 0, 0, 0]));
    }

    #[test]
    fn test_normalize_canvas_preserves_semitransparent_pixels_exactly() {
        let mut rgba_in = RgbaImage::new(6, 6);
        let semi_a = Rgba([123, 45, 67, 128]);
        let semi_b = Rgba([255, 1, 200, 1]);
        rgba_in.put_pixel(0, 0, semi_a);
        rgba_in.put_pixel(5, 5, semi_b);
        let img = DynamicImage::ImageRgba8(rgba_in);

        let out = normalize_canvas_to_cells(&img, FontSize::new(8, 16), Size::new(1, 1));
        assert!(matches!(out, Cow::Owned(_)));
        assert_eq!((out.width(), out.height()), (8, 16));
        let rgba_out = out.as_rgba8().expect("padded canvas is RGBA8");
        assert_eq!(*rgba_out.get_pixel(0, 0), semi_a);
        assert_eq!(*rgba_out.get_pixel(5, 5), semi_b);
        assert_eq!(*rgba_out.get_pixel(6, 5), Rgba([0, 0, 0, 0]));
        assert_eq!(*rgba_out.get_pixel(7, 15), Rgba([0, 0, 0, 0]));
    }

    #[test]
    fn test_empty_protocol_render_is_noop() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 2));
        ViewportProtocol::Empty.render(Rect::new(0, 0, 4, 2), &mut buf);
        assert_eq!(buf[(0, 0)].symbol(), " ");

        let mut out = Vec::new();
        assert!(
            !ViewportProtocol::Empty
                .write_pending_transmit(&mut out)
                .unwrap()
        );
        assert!(out.is_empty());
    }
}
