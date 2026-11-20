//! Parallel `rayon`-accelerated Sixel graphics protocol encoder and renderer.
//!
//! Replaces `ratatui-image`'s single-threaded `icy_sixel` path with a multi-threaded
//! 5-5-5 RGB histogram, Median-Cut quantizer, L1-resident LUT, 4x4 Bayer ordered dithering,
//! and trailing-zero-trimmed 6-row band RLE encoder.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::num::NonZeroU16;

use image::DynamicImage;
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::{Rect, Size};
use rayon::prelude::*;

const UNIT_WIDTH: CellDiffOption = CellDiffOption::ForcedWidth(NonZeroU16::new(1).unwrap());

/// Pixel count threshold above which histogram and band encoding use `rayon` parallelism.
const PARALLEL_PIXEL_THRESHOLD: usize = 65_536;

/// Number of 5-5-5 RGB histogram / LUT entries (`32 * 32 * 32`).
const RGB555_LEN: usize = 32_768;

/// Maximum number of Sixel palette registers emitted.
const MAX_PALETTE_COLORS: usize = 256;

/// Alpha threshold at or above which an RGBA pixel is considered opaque.
const ALPHA_OPAQUE_THRESHOLD: u8 = 128;

/// Centered 4x4 Bayer ordered dithering offsets in `[-4, +4]` with zero DC mean.
///
/// Smoothly eliminates 5-bit quantization banding while remaining 100% parallel across rows.
const BAYER_OFFSET_4X4: [[i16; 4]; 4] = [
    [-4, 0, -3, 1],
    [2, -2, 3, -1],
    [-2, 2, -3, 1],
    [4, 0, 3, -1],
];

/// Parallel Sixel graphics protocol state ready for direct `ratatui` buffer rendering.
pub struct SixelProtocol {
    data: String,
    size: Size,
}

impl SixelProtocol {
    /// Encodes `canvas` into a Sixel escape sequence sized for `size` terminal cells.
    ///
    /// If `canvas` dimensions already match `size * font_size`, avoids any intermediate resize
    /// or full-buffer copy; otherwise crops/pads via `ratatui_image::Resize::Crop(None)`.
    pub fn new(
        canvas: &DynamicImage,
        font_size: ratatui_image::FontSize,
        size: Size,
        is_tmux: bool,
    ) -> Result<Self, String> {
        if size.width == 0 || size.height == 0 {
            return Err("Target size has zero width or height".to_string());
        }
        let target_px_w = u32::from(size.width) * u32::from(font_size.width);
        let target_px_h = u32::from(size.height) * u32::from(font_size.height);

        let normalized;
        let source = if canvas.width() == target_px_w && canvas.height() == target_px_h {
            canvas
        } else {
            normalized = ratatui_image::Resize::Crop(None).resize(canvas, font_size, size, None);
            &normalized
        };

        let width = source.width() as usize;
        let height = source.height() as usize;
        if width == 0 || height == 0 {
            return Err("Source image has zero width or height".to_string());
        }

        let data = match source {
            DynamicImage::ImageRgba8(img) => encode_sixel(
                PixelBuffer::Rgba(img.as_raw()),
                width,
                height,
                size,
                is_tmux,
            ),
            DynamicImage::ImageRgb8(img) => {
                encode_sixel(PixelBuffer::Rgb(img.as_raw()), width, height, size, is_tmux)
            }
            other => {
                let rgba = super::types::to_rgba8_fast(other);
                encode_sixel(
                    PixelBuffer::Rgba(rgba.as_raw()),
                    width,
                    height,
                    size,
                    is_tmux,
                )
            }
        };

        Ok(Self { data, size })
    }

    /// Renders the Sixel escape sequence into the top-left cell of `area` and marks all
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

        let mut skip_first = false;
        for y in render_area.top()..render_area.bottom() {
            for x in render_area.left()..render_area.right() {
                if !skip_first {
                    skip_first = true;
                    continue;
                }
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_diff_option(CellDiffOption::Skip);
                }
            }
        }
    }

    /// Returns the encoded Sixel escape sequence string.
    #[cfg(test)]
    pub fn data(&self) -> &str {
        &self.data
    }

    /// Returns the terminal cell dimensions occupied by this Sixel image.
    #[cfg(test)]
    pub fn size(&self) -> Size {
        self.size
    }
}

#[derive(Clone, Copy)]
enum PixelBuffer<'a> {
    Rgb(&'a [u8]),
    Rgba(&'a [u8]),
}

/// Expands a 5-bit color component (`0..=31`) uniformly to 8-bit (`0..=255`) with exact endpoints.
#[inline]
const fn expand5(v: u8) -> u8 {
    (v << 3) | (v >> 2)
}

/// Computes the 15-bit 5-5-5 RGB index from 8-bit channels.
#[inline]
const fn rgb555_index(r: u8, g: u8, b: u8) -> usize {
    (((r >> 3) as usize) << 10) | (((g >> 3) as usize) << 5) | ((b >> 3) as usize)
}

/// Appends an unsigned integer as ASCII decimal digits without `fmt::Write` overhead.
#[inline]
fn push_u32(buf: &mut Vec<u8>, mut n: u32) {
    if n < 10 {
        buf.push(b'0' + n as u8);
        return;
    }
    let mut tmp = [0u8; 10];
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
        push_u32(out, u32::from(width));
        out.push(b'X');
    } else {
        for _ in 0..height {
            out.extend_from_slice(esc);
            out.push(b'[');
            push_u32(out, u32::from(width));
            out.push(b'X');
            out.extend_from_slice(esc);
            out.extend_from_slice(b"[1B");
        }
        out.extend_from_slice(esc);
        out.push(b'[');
        push_u32(out, u32::from(height));
        out.push(b'A');
    }
}

/// Builds a 5-5-5 RGB histogram (`32,768` entries) over all opaque pixels.
fn build_histogram(pixels: PixelBuffer<'_>, pixel_count: usize) -> Vec<u32> {
    let parallel = pixel_count >= PARALLEL_PIXEL_THRESHOLD;
    match pixels {
        PixelBuffer::Rgba(raw) => {
            if parallel {
                raw.par_chunks(65_536 * 4)
                    .fold(
                        || vec![0u32; RGB555_LEN],
                        |mut hist, chunk| {
                            for &[r, g, b, a] in chunk.as_chunks::<4>().0 {
                                if a >= ALPHA_OPAQUE_THRESHOLD {
                                    hist[rgb555_index(r, g, b)] += 1;
                                }
                            }
                            hist
                        },
                    )
                    .reduce(
                        || vec![0u32; RGB555_LEN],
                        |mut a, b| {
                            for (dst, &src) in a.iter_mut().zip(b.iter()) {
                                *dst += src;
                            }
                            a
                        },
                    )
            } else {
                let mut hist = vec![0u32; RGB555_LEN];
                for &[r, g, b, a] in raw.as_chunks::<4>().0 {
                    if a >= ALPHA_OPAQUE_THRESHOLD {
                        hist[rgb555_index(r, g, b)] += 1;
                    }
                }
                hist
            }
        }
        PixelBuffer::Rgb(raw) => {
            if parallel {
                raw.par_chunks(65_536 * 3)
                    .fold(
                        || vec![0u32; RGB555_LEN],
                        |mut hist, chunk| {
                            for &[r, g, b] in chunk.as_chunks::<3>().0 {
                                hist[rgb555_index(r, g, b)] += 1;
                            }
                            hist
                        },
                    )
                    .reduce(
                        || vec![0u32; RGB555_LEN],
                        |mut a, b| {
                            for (dst, &src) in a.iter_mut().zip(b.iter()) {
                                *dst += src;
                            }
                            a
                        },
                    )
            } else {
                let mut hist = vec![0u32; RGB555_LEN];
                for &[r, g, b] in raw.as_chunks::<3>().0 {
                    hist[rgb555_index(r, g, b)] += 1;
                }
                hist
            }
        }
    }
}

#[derive(Clone, Copy)]
struct HistBin {
    r: u8,
    g: u8,
    b: u8,
    count: u32,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct ColorBox {
    start: usize,
    end: usize,
    total_count: u64,
    widest_channel: u8,
    widest_range: u8,
    priority: u64,
}

impl ColorBox {
    fn from_slice(bins: &[HistBin], start: usize, end: usize) -> Self {
        let mut r_min = 255u8;
        let mut r_max = 0u8;
        let mut g_min = 255u8;
        let mut g_max = 0u8;
        let mut b_min = 255u8;
        let mut b_max = 0u8;
        let mut total_count = 0u64;

        for bin in &bins[start..end] {
            r_min = r_min.min(bin.r);
            r_max = r_max.max(bin.r);
            g_min = g_min.min(bin.g);
            g_max = g_max.max(bin.g);
            b_min = b_min.min(bin.b);
            b_max = b_max.max(bin.b);
            total_count += u64::from(bin.count);
        }

        let r_range = r_max.saturating_sub(r_min);
        let g_range = g_max.saturating_sub(g_min);
        let b_range = b_max.saturating_sub(b_min);

        let (widest_channel, widest_range) = if g_range >= r_range && g_range >= b_range {
            (1, g_range)
        } else if r_range >= b_range {
            (0, r_range)
        } else {
            (2, b_range)
        };

        let priority = if end - start <= 1 || widest_range == 0 {
            0
        } else {
            u64::from(widest_range).saturating_mul(total_count)
        };

        Self {
            start,
            end,
            total_count,
            widest_channel,
            widest_range,
            priority,
        }
    }
}

impl Ord for ColorBox {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority
            .cmp(&other.priority)
            .then_with(|| self.widest_range.cmp(&other.widest_range))
            .then_with(|| self.total_count.cmp(&other.total_count))
            .then_with(|| other.start.cmp(&self.start))
    }
}

impl PartialOrd for ColorBox {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Generates a palette of up to 256 RGB colors using weighted Median-Cut on non-empty histogram bins.
///
/// Returns an empty vector for all-transparent images and an exact unsplit palette when the
/// image contains 256 or fewer unique 5-5-5 colors.
fn build_palette_median_cut(hist: &[u32]) -> Vec<[u8; 3]> {
    let mut bins = Vec::with_capacity(4096);
    for (idx, &count) in hist.iter().enumerate() {
        if count > 0 {
            let r5 = (idx >> 10) as u8;
            let g5 = ((idx >> 5) & 31) as u8;
            let b5 = (idx & 31) as u8;
            bins.push(HistBin {
                r: expand5(r5),
                g: expand5(g5),
                b: expand5(b5),
                count,
            });
        }
    }

    if bins.is_empty() {
        return Vec::new();
    }
    if bins.len() <= MAX_PALETTE_COLORS {
        return bins.iter().map(|b| [b.r, b.g, b.b]).collect();
    }

    let mut heap = BinaryHeap::with_capacity(MAX_PALETTE_COLORS);
    heap.push(ColorBox::from_slice(&bins, 0, bins.len()));

    while heap.len() < MAX_PALETTE_COLORS {
        let Some(cbox) = heap.pop() else {
            break;
        };
        if cbox.end - cbox.start <= 1 || cbox.widest_range == 0 {
            heap.push(cbox);
            break;
        }

        let slice = &mut bins[cbox.start..cbox.end];
        match cbox.widest_channel {
            0 => slice.sort_unstable_by_key(|b| b.r),
            1 => slice.sort_unstable_by_key(|b| b.g),
            _ => slice.sort_unstable_by_key(|b| b.b),
        }

        let half = cbox.total_count / 2;
        let mut acc = 0u64;
        let mut split_offset = 1usize;
        for (i, bin) in slice.iter().enumerate() {
            acc += u64::from(bin.count);
            if acc >= half {
                split_offset = (i + 1).clamp(1, slice.len() - 1);
                break;
            }
        }

        let mid = cbox.start + split_offset;
        heap.push(ColorBox::from_slice(&bins, cbox.start, mid));
        heap.push(ColorBox::from_slice(&bins, mid, cbox.end));
    }

    let mut boxes: Vec<ColorBox> = heap.into_vec();
    boxes.sort_unstable_by_key(|b| b.start);

    let mut palette = Vec::with_capacity(boxes.len());
    for cbox in boxes {
        let mut sum_r = 0u64;
        let mut sum_g = 0u64;
        let mut sum_b = 0u64;
        for bin in &bins[cbox.start..cbox.end] {
            let w = u64::from(bin.count);
            sum_r += u64::from(bin.r) * w;
            sum_g += u64::from(bin.g) * w;
            sum_b += u64::from(bin.b) * w;
        }
        let denom = cbox.total_count.max(1);
        let half = denom / 2;
        let r = ((sum_r + half) / denom).min(255) as u8;
        let g = ((sum_g + half) / denom).min(255) as u8;
        let b = ((sum_b + half) / denom).min(255) as u8;
        palette.push([r, g, b]);
    }

    palette
}

/// Builds a 32 KB L1-resident LUT mapping every 5-5-5 RGB color to its nearest palette index.
fn build_rgb555_lut(palette: &[[u8; 3]]) -> Vec<u8> {
    let mut lut = vec![0u8; RGB555_LEN];
    if palette.len() <= 1 {
        return lut;
    }

    let pal_r: Vec<i32> = palette.iter().map(|c| i32::from(c[0])).collect();
    let pal_g: Vec<i32> = palette.iter().map(|c| i32::from(c[1])).collect();
    let pal_b: Vec<i32> = palette.iter().map(|c| i32::from(c[2])).collect();
    let pal_len = palette.len();

    let fill_chunk = |chunk_idx: usize, chunk: &mut [u8]| {
        let base = chunk_idx * 1024;
        for (offset, dst) in chunk.iter_mut().enumerate() {
            let idx = base + offset;
            let r = i32::from(expand5((idx >> 10) as u8));
            let g = i32::from(expand5(((idx >> 5) & 31) as u8));
            let b = i32::from(expand5((idx & 31) as u8));

            let mut best_idx = 0usize;
            let mut best_dist = i32::MAX;
            for i in 0..pal_len {
                let dr = r - pal_r[i];
                let dg = g - pal_g[i];
                let db = b - pal_b[i];
                let dist = dr * dr + dg * dg + db * db;
                if dist < best_dist {
                    best_dist = dist;
                    best_idx = i;
                    if dist == 0 {
                        break;
                    }
                }
            }
            *dst = best_idx as u8;
        }
    };

    if pal_len > 16 {
        lut.par_chunks_mut(1024)
            .enumerate()
            .for_each(|(chunk_idx, chunk)| fill_chunk(chunk_idx, chunk));
    } else {
        for (chunk_idx, chunk) in lut.chunks_mut(1024).enumerate() {
            fill_chunk(chunk_idx, chunk);
        }
    }

    lut
}

/// Reusable per-worker scratch buffers for zero-allocation 6-row band encoding.
struct BandScratch {
    color_to_slot: [u16; MAX_PALETTE_COLORS],
    used_colors: Vec<u8>,
    max_x: [usize; MAX_PALETTE_COLORS],
    masks: Vec<u8>,
}

impl BandScratch {
    fn new(width: usize) -> Self {
        Self {
            color_to_slot: [u16::MAX; MAX_PALETTE_COLORS],
            used_colors: Vec::with_capacity(64),
            max_x: [0; MAX_PALETTE_COLORS],
            masks: vec![0u8; width * 16],
        }
    }

    #[inline]
    fn ensure_slot(&mut self, color: u8, width: usize) -> usize {
        let existing = self.color_to_slot[usize::from(color)];
        if existing != u16::MAX {
            return usize::from(existing);
        }
        let slot = self.used_colors.len();
        self.color_to_slot[usize::from(color)] = slot as u16;
        self.used_colors.push(color);
        let needed = (slot + 1) * width;
        if self.masks.len() < needed {
            self.masks.resize(needed, 0);
        }
        slot
    }

    /// Encodes one 6-row band into ASCII Sixel bytes, trimming trailing blank sixels per color.
    fn encode_band(
        &mut self,
        pixels: PixelBuffer<'_>,
        width: usize,
        height: usize,
        band_idx: usize,
        lut: &[u8],
    ) -> Vec<u8> {
        let y_start = band_idx * 6;
        let rows_in_band = (height - y_start).min(6);

        match pixels {
            PixelBuffer::Rgba(raw) => {
                let row_stride = width * 4;
                for dy in 0..rows_in_band {
                    let y = y_start + dy;
                    let bit = 1u8 << dy;
                    let bayer_row = &BAYER_OFFSET_4X4[y & 3];
                    let row = &raw[y * row_stride..(y + 1) * row_stride];
                    for (x, &[pr, pg, pb, pa]) in row.as_chunks::<4>().0.iter().enumerate() {
                        if pa < ALPHA_OPAQUE_THRESHOLD {
                            continue;
                        }
                        let d = bayer_row[x & 3];
                        let r = (i16::from(pr) + d).clamp(0, 255) as u8;
                        let g = (i16::from(pg) + d).clamp(0, 255) as u8;
                        let b = (i16::from(pb) + d).clamp(0, 255) as u8;
                        let color = lut[rgb555_index(r, g, b)];
                        let slot = self.ensure_slot(color, width);
                        self.masks[slot * width + x] |= bit;
                        if x + 1 > self.max_x[slot] {
                            self.max_x[slot] = x + 1;
                        }
                    }
                }
            }
            PixelBuffer::Rgb(raw) => {
                let row_stride = width * 3;
                for dy in 0..rows_in_band {
                    let y = y_start + dy;
                    let bit = 1u8 << dy;
                    let bayer_row = &BAYER_OFFSET_4X4[y & 3];
                    let row = &raw[y * row_stride..(y + 1) * row_stride];
                    for (x, &[pr, pg, pb]) in row.as_chunks::<3>().0.iter().enumerate() {
                        let d = bayer_row[x & 3];
                        let r = (i16::from(pr) + d).clamp(0, 255) as u8;
                        let g = (i16::from(pg) + d).clamp(0, 255) as u8;
                        let b = (i16::from(pb) + d).clamp(0, 255) as u8;
                        let color = lut[rgb555_index(r, g, b)];
                        let slot = self.ensure_slot(color, width);
                        self.masks[slot * width + x] |= bit;
                        if x + 1 > self.max_x[slot] {
                            self.max_x[slot] = x + 1;
                        }
                    }
                }
            }
        }

        if self.used_colors.is_empty() {
            return Vec::new();
        }

        let mut out = Vec::with_capacity(self.used_colors.len() * 16 + width);
        for slot in 0..self.used_colors.len() {
            let color = self.used_colors[slot];
            let len = self.max_x[slot];
            let base = slot * width;
            let row_masks = &mut self.masks[base..base + len];

            if slot > 0 {
                out.push(b'$');
            }
            out.push(b'#');
            push_u32(&mut out, u32::from(color));

            let mut i = 0;
            while i < len {
                let val = row_masks[i];
                let mut run = 1usize;
                while i + run < len && row_masks[i + run] == val {
                    run += 1;
                }
                let ch = val + 63;
                if run > 3 {
                    out.push(b'!');
                    push_u32(&mut out, run as u32);
                    out.push(ch);
                } else {
                    for _ in 0..run {
                        out.push(ch);
                    }
                }
                i += run;
            }

            row_masks.fill(0);
            self.color_to_slot[usize::from(color)] = u16::MAX;
            self.max_x[slot] = 0;
        }
        self.used_colors.clear();

        out
    }
}

/// Full Sixel encoding pipeline from raw RGB/RGBA pixels to a ready-to-render escape sequence.
fn encode_sixel(
    pixels: PixelBuffer<'_>,
    width: usize,
    height: usize,
    size: Size,
    is_tmux: bool,
) -> String {
    let pixel_count = width * height;
    let hist = build_histogram(pixels, pixel_count);
    let palette = build_palette_median_cut(&hist);

    let (start, escape, end) =
        ratatui_image::picker::cap_parser::Parser::tmux_start_escape_end(is_tmux);

    let mut header = Vec::with_capacity(128 + palette.len() * 20 + usize::from(size.height) * 16);
    header.extend_from_slice(start.as_bytes());
    write_clear_area(&mut header, escape, size.width, size.height);
    header.extend_from_slice(escape.as_bytes());
    header.extend_from_slice(b"P0;1;0q\"1;1;");
    push_u32(&mut header, width as u32);
    header.push(b';');
    push_u32(&mut header, height as u32);

    for (idx, &[r, g, b]) in palette.iter().enumerate() {
        header.push(b'#');
        push_u32(&mut header, idx as u32);
        header.extend_from_slice(b";2;");
        push_u32(&mut header, (u32::from(r) * 100 + 127) / 255);
        header.push(b';');
        push_u32(&mut header, (u32::from(g) * 100 + 127) / 255);
        header.push(b';');
        push_u32(&mut header, (u32::from(b) * 100 + 127) / 255);
    }

    let mut trailer = Vec::with_capacity(escape.len() + 1 + end.len());
    trailer.extend_from_slice(escape.as_bytes());
    trailer.push(b'\\');
    trailer.extend_from_slice(end.as_bytes());

    if palette.is_empty() {
        let mut out = Vec::with_capacity(header.len() + trailer.len());
        out.extend_from_slice(&header);
        out.extend_from_slice(&trailer);
        return String::from_utf8(out).expect("sixel output is valid ASCII");
    }

    let lut = build_rgb555_lut(&palette);
    let num_bands = height.div_ceil(6);

    let bands: Vec<Vec<u8>> = if pixel_count >= PARALLEL_PIXEL_THRESHOLD && num_bands > 1 {
        (0..num_bands)
            .into_par_iter()
            .map_init(
                || BandScratch::new(width),
                |scratch, band_idx| scratch.encode_band(pixels, width, height, band_idx, &lut),
            )
            .collect()
    } else {
        let mut scratch = BandScratch::new(width);
        (0..num_bands)
            .map(|band_idx| scratch.encode_band(pixels, width, height, band_idx, &lut))
            .collect()
    };

    let bands_bytes: usize = bands.iter().map(Vec::len).sum();
    let separators = num_bands.saturating_sub(1);
    let mut out = Vec::with_capacity(header.len() + bands_bytes + separators + trailer.len());
    out.extend_from_slice(&header);
    for (idx, band) in bands.iter().enumerate() {
        if idx > 0 {
            out.push(b'-');
        }
        out.extend_from_slice(band);
    }
    out.extend_from_slice(&trailer);

    String::from_utf8(out).expect("sixel output is valid ASCII")
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage, Rgba, RgbaImage};
    use ratatui_image::FontSize;

    fn unit_font() -> FontSize {
        FontSize {
            width: 1,
            height: 1,
        }
    }

    /// Minimal Sixel decoder for round-trip structural and pixel validation in tests.
    fn decode_sixel_body(data: &str) -> (u32, u32, Vec<[u8; 3]>, Vec<Option<usize>>) {
        let dcs_pos = data.find("P0;1;0q").expect("Missing Sixel DCS introducer");
        let after_q = &data[dcs_pos + "P0;1;0q".len()..];
        let st_pos = after_q.find('\x1b').expect("Missing ST escape terminator");
        let body = &after_q[..st_pos];

        let bytes = body.as_bytes();
        let mut i = 0;

        // Parse raster attributes "1;1;w;h
        assert_eq!(bytes[i], b'"', "Expected raster attributes");
        i += 1;
        let mut nums = Vec::new();
        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b';') {
            if bytes[i] == b';' {
                i += 1;
                continue;
            }
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let val: u32 = std::str::from_utf8(&bytes[start..i])
                .unwrap()
                .parse()
                .unwrap();
            nums.push(val);
        }
        assert_eq!(nums.len(), 4, "Raster attributes must have 4 numbers");
        assert_eq!((nums[0], nums[1]), (1, 1));
        let (w, h) = (nums[2], nums[3]);

        let mut palette: Vec<[u8; 3]> = Vec::new();
        let mut pixels: Vec<Option<usize>> = vec![None; (w * h) as usize];
        let mut cur_color = 0usize;
        let mut x = 0usize;
        let mut band_y = 0usize;

        while i < bytes.len() {
            match bytes[i] {
                b'#' => {
                    i += 1;
                    let start = i;
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                    let idx: usize = std::str::from_utf8(&bytes[start..i])
                        .unwrap()
                        .parse()
                        .unwrap();
                    if i < bytes.len() && bytes[i] == b';' {
                        // Palette definition #i;2;r;g;b
                        let mut params = Vec::new();
                        while i < bytes.len() && bytes[i] == b';' {
                            i += 1;
                            let s = i;
                            while i < bytes.len() && bytes[i].is_ascii_digit() {
                                i += 1;
                            }
                            params.push(
                                std::str::from_utf8(&bytes[s..i])
                                    .unwrap()
                                    .parse::<u8>()
                                    .unwrap(),
                            );
                        }
                        assert_eq!(params.len(), 4);
                        assert_eq!(params[0], 2);
                        if palette.len() <= idx {
                            palette.resize(idx + 1, [0, 0, 0]);
                        }
                        palette[idx] = [params[1], params[2], params[3]];
                    } else {
                        cur_color = idx;
                    }
                }
                b'$' => {
                    x = 0;
                    i += 1;
                }
                b'-' => {
                    x = 0;
                    band_y += 6;
                    i += 1;
                }
                b'!' => {
                    i += 1;
                    let s = i;
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                    let count: usize = std::str::from_utf8(&bytes[s..i]).unwrap().parse().unwrap();
                    let sixel = bytes[i] - 63;
                    i += 1;
                    for _ in 0..count {
                        for bit in 0..6 {
                            if (sixel & (1 << bit)) != 0
                                && band_y + bit < h as usize
                                && x < w as usize
                            {
                                pixels[(band_y + bit) * (w as usize) + x] = Some(cur_color);
                            }
                        }
                        x += 1;
                    }
                }
                b'?'..=b'~' => {
                    let sixel = bytes[i] - 63;
                    i += 1;
                    for bit in 0..6 {
                        if (sixel & (1 << bit)) != 0 && band_y + bit < h as usize && x < w as usize
                        {
                            pixels[(band_y + bit) * (w as usize) + x] = Some(cur_color);
                        }
                    }
                    x += 1;
                }
                other => panic!(
                    "Unexpected Sixel byte: 0x{:02X} ('{}')",
                    other, other as char
                ),
            }
        }

        (w, h, palette, pixels)
    }

    #[test]
    fn test_sixel_single_color_rgb_and_structure() {
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(16, 12, Rgb([255, 0, 0])));
        let proto = SixelProtocol::new(&img, unit_font(), Size::new(16, 12), false)
            .expect("Sixel encode failed");

        let data = proto.data();
        assert!(data.contains("\x1bP0;1;0q\"1;1;16;12"));
        assert!(data.contains("#0;2;100;0;0"));
        assert!(data.ends_with("\x1b\\"));

        let (w, h, palette, pixels) = decode_sixel_body(data);
        assert_eq!((w, h), (16, 12));
        assert_eq!(palette, vec![[100, 0, 0]]);
        assert!(pixels.iter().all(|&p| p == Some(0)));
    }

    #[test]
    fn test_sixel_1x1_and_odd_heights() {
        for &h in &[1u32, 5, 6, 7, 13] {
            let w = if h == 1 { 1 } else { 9 };
            let img = DynamicImage::ImageRgba8(RgbaImage::from_pixel(w, h, Rgba([0, 255, 0, 255])));
            let proto = SixelProtocol::new(&img, unit_font(), Size::new(w as u16, h as u16), false)
                .expect("Sixel encode failed");

            let (dw, dh, palette, pixels) = decode_sixel_body(proto.data());
            assert_eq!((dw, dh), (w, h));
            assert_eq!(palette, vec![[0, 100, 0]]);
            assert!(
                pixels.iter().all(|&p| p == Some(0)),
                "All pixels must be filled for height={h}"
            );

            // Band separator count must equal ceil(h / 6) - 1
            let dcs_pos = proto.data().find("P0;1;0q").unwrap();
            let body = &proto.data()[dcs_pos..];
            let expected_seps = h.div_ceil(6).saturating_sub(1) as usize;
            let actual_seps = body.bytes().filter(|&b| b == b'-').count();
            assert_eq!(
                actual_seps, expected_seps,
                "Wrong band count for height={h}"
            );
        }
    }

    #[test]
    fn test_sixel_transparency_and_all_transparent_image() {
        // 1. All-transparent image -> minimal valid Sixel sequence with 0 palette entries
        let empty = DynamicImage::ImageRgba8(RgbaImage::from_pixel(8, 8, Rgba([100, 200, 50, 0])));
        let proto_empty = SixelProtocol::new(&empty, unit_font(), Size::new(8, 8), false)
            .expect("Transparent Sixel encode failed");
        let (w, h, palette, pixels) = decode_sixel_body(proto_empty.data());
        assert_eq!((w, h), (8, 8));
        assert!(palette.is_empty());
        assert!(pixels.iter().all(|p| p.is_none()));

        // 2. Half-transparent image: left 4 cols opaque blue, right 4 cols transparent
        let mut rgba = RgbaImage::from_pixel(8, 6, Rgba([0, 0, 0, 0]));
        for y in 0..6 {
            for x in 0..4 {
                rgba.put_pixel(x, y, Rgba([0, 0, 255, 255]));
            }
        }
        let img = DynamicImage::ImageRgba8(rgba);
        let proto = SixelProtocol::new(&img, unit_font(), Size::new(8, 6), false)
            .expect("Partial transparent Sixel encode failed");
        let (w, h, palette, pixels) = decode_sixel_body(proto.data());
        assert_eq!((w, h), (8, 6));
        assert_eq!(palette, vec![[0, 0, 100]]);
        for y in 0..6 {
            for x in 0..8 {
                let expected = if x < 4 { Some(0) } else { None };
                assert_eq!(pixels[y * 8 + x], expected);
            }
        }
    }

    #[test]
    fn test_sixel_trailing_zero_trimming() {
        // Left 2 columns are pure red, remaining 98 columns are pure blue.
        // Red should stop at x=2 and NOT emit trailing '?' padding across columns 2..100.
        let mut rgb = RgbImage::from_pixel(100, 6, Rgb([0, 0, 255]));
        for y in 0..6 {
            rgb.put_pixel(0, y, Rgb([255, 0, 0]));
            rgb.put_pixel(1, y, Rgb([255, 0, 0]));
        }
        let img = DynamicImage::ImageRgb8(rgb);
        let proto = SixelProtocol::new(&img, unit_font(), Size::new(100, 6), false)
            .expect("Sixel encode failed");

        let data = proto.data();
        // After the palette definitions, the first color emitted is red at x=0..2 -> "#c~~$"
        // with NO trailing '?' characters before '$'.
        let dollar_pos = data.find('$').expect("Expected '$' between two colors");
        let before_dollar = &data[..dollar_pos];
        assert!(
            before_dollar.ends_with("~~"),
            "Expected red band to end immediately after '~~' without trailing '?' padding, got: {before_dollar}"
        );
        assert!(
            !before_dollar.ends_with('?'),
            "Trailing empty sixels should be trimmed"
        );

        // Verify decoded pixels are exact
        let (w, h, palette, pixels) = decode_sixel_body(data);
        assert_eq!((w, h), (100, 6));
        assert_eq!(palette.len(), 2);
        for y in 0..6 {
            for x in 0..100 {
                let idx = pixels[y * 100 + x].expect("Opaque pixel");
                let expected = if x < 2 { [100, 0, 0] } else { [0, 0, 100] };
                assert_eq!(palette[idx], expected);
            }
        }
    }

    #[test]
    fn test_sixel_gradient_and_high_color_count_parallel_path() {
        // 320x240 = 76,800 pixels (> PARALLEL_PIXEL_THRESHOLD) with > 256 unique RGB colors
        let mut rgba = RgbaImage::new(320, 240);
        for y in 0..240 {
            for x in 0..320 {
                let r = ((x * 255) / 319) as u8;
                let g = ((y * 255) / 239) as u8;
                let b = (((x + y) * 255) / 558) as u8;
                rgba.put_pixel(x, y, Rgba([r, g, b, 255]));
            }
        }
        let img = DynamicImage::ImageRgba8(rgba);
        let font = FontSize {
            width: 8,
            height: 16,
        };
        let size = Size::new(40, 15);
        let proto = SixelProtocol::new(&img, font, size, false).expect("Parallel Sixel failed");

        let (w, h, palette, pixels) = decode_sixel_body(proto.data());
        assert_eq!((w, h), (320, 240));
        assert!(
            !palette.is_empty() && palette.len() <= MAX_PALETTE_COLORS,
            "Palette size {} out of bounds",
            palette.len()
        );
        assert!(
            pixels.iter().all(|p| p.is_some()),
            "Every pixel in opaque gradient must be encoded"
        );
    }

    #[test]
    fn test_sixel_tmux_passthrough_framing() {
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 6, Rgb([255, 255, 255])));
        let proto = SixelProtocol::new(&img, unit_font(), Size::new(4, 6), true)
            .expect("Tmux Sixel encode failed");
        let data = proto.data();

        assert!(
            data.starts_with("\x1bPtmux;"),
            "Expected tmux DCS passthrough start"
        );
        assert!(
            data.contains("\x1b\x1bP0;1;0q\"1;1;4;6"),
            "Expected doubled ESC before Sixel DCS inside tmux passthrough"
        );
        assert!(
            data.ends_with("\x1b\x1b\\\x1b\\"),
            "Expected escaped inner ST followed by outer tmux ST"
        );
    }

    #[test]
    fn test_sixel_protocol_render_buffer_and_bounds() {
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 6, Rgb([255, 0, 0])));
        let font = FontSize {
            width: 2,
            height: 3,
        };
        let size = Size::new(2, 2);
        let proto = SixelProtocol::new(&img, font, size, false).expect("Sixel encode failed");
        assert_eq!(proto.size(), size);

        // 1. Render into a larger area
        let area = Rect::new(1, 1, 4, 4);
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 6));
        proto.render(area, &mut buf);

        assert_eq!(buf[(1, 1)].symbol(), proto.data());
        assert_eq!(buf[(1, 1)].diff_option, UNIT_WIDTH);
        assert_eq!(buf[(2, 1)].diff_option, CellDiffOption::Skip);
        assert_eq!(buf[(1, 2)].diff_option, CellDiffOption::Skip);
        assert_eq!(buf[(2, 2)].diff_option, CellDiffOption::Skip);
        // Cell outside render_area remains untouched
        assert_eq!(buf[(3, 1)].diff_option, CellDiffOption::default());

        // 2. Oversized image relative to area must safely no-op
        let small_area = Rect::new(0, 0, 1, 2);
        let mut small_buf = Buffer::empty(small_area);
        proto.render(small_area, &mut small_buf);
        assert_eq!(small_buf[(0, 0)].symbol(), " ");
        assert_eq!(small_buf[(0, 0)].diff_option, CellDiffOption::default());
    }

    #[test]
    fn test_sixel_zero_size_returns_error() {
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(4, 4, Rgb([0, 0, 0])));
        assert!(SixelProtocol::new(&img, unit_font(), Size::new(0, 4), false).is_err());
        assert!(SixelProtocol::new(&img, unit_font(), Size::new(4, 0), false).is_err());
    }
}
