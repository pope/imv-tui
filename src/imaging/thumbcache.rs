//! Freedesktop.org Thumbnail Managing Standard (v0.8.0) implementation.
//!
//! Provides thumbnail generation, RFC 1321 MD5 URI hashing, PNG `tEXt` metadata chunk injection,
//! and non-blocking multi-tier caching adhering to the XDG Freedesktop thumbnail specification.

use fast_image_resize as fir;
use image::DynamicImage;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

/// 64-round constants for RFC 1321 MD5.
const K: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
    0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
    0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
    0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
    0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
    0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
    0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
    0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
];

/// Per-round bit shift amounts for RFC 1321 MD5.
const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

/// Computes the 32-character lowercase hexadecimal MD5 digest of `data` per RFC 1321 in pure safe Rust.
pub fn md5_hex(data: &[u8]) -> String {
    let mut a0 = 0x67452301u32;
    let mut b0 = 0xefcdab89u32;
    let mut c0 = 0x98badcfeu32;
    let mut d0 = 0x10325476u32;

    let orig_len_bits = (data.len() as u64).wrapping_mul(8);

    // Padding: append 0x80, then pad with 0s until len % 64 == 56, then 8 bytes of orig_len_bits
    let mut padded = Vec::with_capacity(data.len() + 64);
    padded.extend_from_slice(data);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0x00);
    }
    padded.extend_from_slice(&orig_len_bits.to_le_bytes());

    for chunk in padded.as_chunks::<64>().0 {
        let mut m = [0u32; 16];
        for (i, word) in m.iter_mut().enumerate() {
            *word = u32::from_le_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap());
        }

        let mut a = a0;
        let mut b = b0;
        let mut c = c0;
        let mut d = d0;

        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | (!b & d), i),
                16..=31 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };

            let temp = d;
            d = c;
            c = b;
            let sum = a.wrapping_add(f).wrapping_add(K[i]).wrapping_add(m[g]);
            b = b.wrapping_add(sum.rotate_left(S[i]));
            a = temp;
        }

        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut digest = [0u8; 16];
    digest[0..4].copy_from_slice(&a0.to_le_bytes());
    digest[4..8].copy_from_slice(&b0.to_le_bytes());
    digest[8..12].copy_from_slice(&c0.to_le_bytes());
    digest[12..16].copy_from_slice(&d0.to_le_bytes());

    let mut hex = String::with_capacity(32);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(hex, "{:02x}", byte);
    }
    hex
}

/// Precomputed lookup table for IEEE 802.3 CRC32 checksums.
const fn make_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xedb88320;
            } else {
                crc >>= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

const CRC_TABLE: [u32; 256] = make_crc_table();

/// Calculates the standard IEEE 802.3 CRC32 checksum for `chunk_type` followed by `data`.
pub fn crc32_chunk(chunk_type: &[u8; 4], data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in chunk_type {
        let idx = ((crc ^ (byte as u32)) & 0xff) as usize;
        crc = (crc >> 8) ^ CRC_TABLE[idx];
    }
    for &byte in data {
        let idx = ((crc ^ (byte as u32)) & 0xff) as usize;
        crc = (crc >> 8) ^ CRC_TABLE[idx];
    }
    crc ^ 0xffff_ffff
}

/// Freedesktop thumbnail size tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbTier {
    /// 128x128 max dimension.
    Normal,
    /// 256x256 max dimension.
    Large,
    /// 512x512 max dimension.
    XLarge,
    /// 1024x1024 max dimension.
    XXLarge,
}

impl ThumbTier {
    /// Maximum dimension (width or height) in pixels.
    pub const fn max_dim(self) -> u32 {
        match self {
            Self::Normal => 128,
            Self::Large => 256,
            Self::XLarge => 512,
            Self::XXLarge => 1024,
        }
    }

    /// Freedesktop cache subdirectory name.
    pub const fn dir_name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Large => "large",
            Self::XLarge => "x-large",
            Self::XXLarge => "xx-large",
        }
    }
}

/// Computes the Freedesktop cache key (canonical file URI and MD5 filename) for a given filesystem path.
///
/// Uses `std::path::absolute` to preserve symlink paths rather than `canonicalize` per the Freedesktop spec.
pub fn cache_key_for_path(path: &Path) -> Option<(String, String)> {
    let abs_path = std::path::absolute(path).ok()?;
    let path_str = abs_path.to_str()?;

    // Build RFC 3986 URI: "file://" + percent-encoded path
    let mut uri = String::with_capacity(7 + path_str.len() * 2);
    uri.push_str("file://");

    for &byte in path_str.as_bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                uri.push(byte as char);
            }
            other => {
                use std::fmt::Write;
                let _ = write!(uri, "%{:02X}", other);
            }
        }
    }

    let md5 = md5_hex(uri.as_bytes());
    let filename = format!("{}.png", md5);
    Some((uri, filename))
}

/// Computes aspect-ratio preserving target dimensions within `max_dim x max_dim` without upscaling.
pub fn compute_scaled_dims(src_w: u32, src_h: u32, max_dim: u32) -> (u32, u32) {
    if src_w == 0 || src_h == 0 {
        return (0, 0);
    }
    if src_w <= max_dim && src_h <= max_dim {
        return (src_w, src_h);
    }
    let s_w = max_dim as f64 / src_w as f64;
    let s_h = max_dim as f64 / src_h as f64;
    let s = s_w.min(s_h);
    let dst_w = (src_w as f64 * s).round().max(1.0) as u32;
    let dst_h = (src_h as f64 * s).round().max(1.0) as u32;
    (dst_w, dst_h)
}

/// Downscales `src_img` to `target_tier` using Mitchell-Netravali filtering with alpha premultiplication.
pub fn downscale_for_thumbnail(
    resizer: &mut fir::Resizer,
    src_img: &DynamicImage,
    target_tier: ThumbTier,
) -> Result<image::RgbaImage, String> {
    let src_w = src_img.width();
    let src_h = src_img.height();
    if src_w == 0 || src_h == 0 {
        return Err("Source image dimensions are zero".to_string());
    }

    let (target_w, target_h) = compute_scaled_dims(src_w, src_h, target_tier.max_dim());

    match src_img {
        DynamicImage::ImageRgb8(rgb_src) => {
            if target_w == src_w && target_h == src_h {
                let rgba_raw = crate::imaging::unpack_rgb24_to_rgba32(rgb_src);
                return image::RgbaImage::from_raw(target_w, target_h, rgba_raw).ok_or_else(|| {
                    "Failed to construct RgbaImage from unpacked buffer".to_string()
                });
            }

            let mut dst_image = fir::images::Image::new(target_w, target_h, fir::PixelType::U8x3);

            let mut options = fir::ResizeOptions::new();
            options.algorithm =
                crate::imaging::to_fir_resize_alg(crate::imaging::FilterType::Mitchell);
            options.mul_div_alpha = false;

            resizer
                .resize(rgb_src, &mut dst_image, Some(&options))
                .map_err(|e| format!("Thumbnail resize failed: {}", e))?;

            let rgb_dst = image::RgbImage::from_raw(target_w, target_h, dst_image.into_vec())
                .ok_or_else(|| "Failed to construct RgbImage from resized buffer".to_string())?;

            let rgba_raw = crate::imaging::unpack_rgb24_to_rgba32(&rgb_dst);
            image::RgbaImage::from_raw(target_w, target_h, rgba_raw)
                .ok_or_else(|| "Failed to construct RgbaImage from unpacked buffer".to_string())
        }
        _ => {
            let src_rgba = crate::imaging::to_rgba8_fast(src_img);

            if target_w == src_w && target_h == src_h {
                return Ok(src_rgba.into_owned());
            }

            let mut dst_image = fir::images::Image::new(target_w, target_h, fir::PixelType::U8x4);

            let mut options = fir::ResizeOptions::new();
            options.algorithm =
                crate::imaging::to_fir_resize_alg(crate::imaging::FilterType::Mitchell);
            options.mul_div_alpha = true;

            resizer
                .resize(src_rgba.as_ref(), &mut dst_image, Some(&options))
                .map_err(|e| format!("Thumbnail resize failed: {}", e))?;

            let dst_raw = dst_image.into_vec();
            image::RgbaImage::from_raw(target_w, target_h, dst_raw)
                .ok_or_else(|| "Failed to construct RgbaImage from resized buffer".to_string())
        }
    }
}

/// Metadata parsed from a cached Freedesktop PNG header (`IHDR` + `tEXt` chunks).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PngThumbMetadata {
    /// Encoded width of the thumbnail PNG from `IHDR`.
    pub thumb_w: u32,
    /// Encoded height of the thumbnail PNG from `IHDR`.
    pub thumb_h: u32,
    /// Modification timestamp (`Thumb::MTime`) in seconds since `UNIX_EPOCH`.
    pub mtime_sec: Option<i64>,
    /// Original full-resolution image dimensions `(width, height)` from `Thumb::Image::Width`/`Height`.
    pub source_dims: Option<(u32, u32)>,
}

/// Metadata written into Freedesktop PNG `tEXt` chunks.
#[derive(Debug, Clone, Copy)]
pub struct WriteThumbMetadata<'a> {
    /// Canonical `file://` URI of the source image.
    pub canonical_uri: &'a str,
    /// Source file modification timestamp in seconds since `UNIX_EPOCH`.
    pub mtime_sec: i64,
    /// Optional source file size in bytes (`Thumb::Size`).
    pub file_size: Option<u64>,
    /// Optional original full-resolution dimensions `(width, height)` (`Thumb::Image::Width` / `Height`).
    pub source_dims: Option<(u32, u32)>,
}

/// Decoded Freedesktop cached thumbnail image and optional original source dimensions.
pub struct CachedThumbnail {
    /// Decoded thumbnail image.
    pub image: DynamicImage,
    /// Original full-resolution image dimensions `(width, height)` stored in PNG `tEXt` metadata, if present.
    pub source_dims: Option<(u32, u32)>,
}

/// Constructs a PNG `tEXt` chunk from a keyword and text value.
fn make_text_chunk(keyword: &str, text: &str) -> Vec<u8> {
    let mut data = Vec::with_capacity(keyword.len() + 1 + text.len());
    data.extend_from_slice(keyword.as_bytes());
    data.push(0x00);
    data.extend_from_slice(text.as_bytes());

    let mut chunk = Vec::with_capacity(12 + data.len());
    chunk.extend_from_slice(&(data.len() as u32).to_be_bytes());
    chunk.extend_from_slice(b"tEXt");
    chunk.extend_from_slice(&data);

    let crc = crc32_chunk(b"tEXt", &data);
    chunk.extend_from_slice(&crc.to_be_bytes());

    chunk
}

/// Encodes an RGBA8 thumbnail image to PNG bytes with injected Freedesktop `tEXt` chunks
/// containing URI, modification time, file size, software, and source dimensions.
pub fn encode_thumbnail_png_with_metadata(
    thumb_rgba: &image::RgbaImage,
    meta: WriteThumbMetadata<'_>,
) -> Result<Vec<u8>, String> {
    let mut png_bytes = Vec::new();
    thumb_rgba
        .write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
        .map_err(|e| format!("PNG encoding failed: {}", e))?;

    if png_bytes.len() < 33 || &png_bytes[0..8] != b"\x89PNG\r\n\x1a\n" {
        return Err("Invalid PNG generated".to_string());
    }

    let chunk_sw = make_text_chunk("Software", "imv-tui");
    let chunk_uri = make_text_chunk("Thumb::URI", meta.canonical_uri);
    let chunk_mtime = make_text_chunk("Thumb::MTime", &meta.mtime_sec.to_string());

    let chunk_size = meta
        .file_size
        .map(|s| make_text_chunk("Thumb::Size", &s.to_string()));

    let (chunk_w, chunk_h) = if let Some((w, h)) = meta.source_dims
        && w > 0
        && h > 0
    {
        (
            Some(make_text_chunk("Thumb::Image::Width", &w.to_string())),
            Some(make_text_chunk("Thumb::Image::Height", &h.to_string())),
        )
    } else {
        (None, None)
    };

    let extra_len = chunk_sw.len()
        + chunk_uri.len()
        + chunk_mtime.len()
        + chunk_size.as_ref().map(|c| c.len()).unwrap_or(0)
        + chunk_w.as_ref().map(|c| c.len()).unwrap_or(0)
        + chunk_h.as_ref().map(|c| c.len()).unwrap_or(0);

    let mut result = Vec::with_capacity(png_bytes.len() + extra_len);
    // Standard PNG header + IHDR chunk is exactly 33 bytes
    result.extend_from_slice(&png_bytes[..33]);
    result.extend_from_slice(&chunk_sw);
    result.extend_from_slice(&chunk_uri);
    result.extend_from_slice(&chunk_mtime);
    if let Some(ref c) = chunk_size {
        result.extend_from_slice(c);
    }
    if let Some(ref c) = chunk_w {
        result.extend_from_slice(c);
    }
    if let Some(ref c) = chunk_h {
        result.extend_from_slice(c);
    }
    result.extend_from_slice(&png_bytes[33..]);

    Ok(result)
}

/// Encodes an RGBA8 thumbnail image to PNG bytes with injected Freedesktop `tEXt` chunks.
///
/// Guaranteed to position `Thumb::MTime` within the first 2,048 bytes of the file.
#[cfg(test)]
pub fn encode_thumbnail_png(
    thumb_rgba: &image::RgbaImage,
    canonical_uri: &str,
    mtime_sec: i64,
) -> Result<Vec<u8>, String> {
    encode_thumbnail_png_with_metadata(
        thumb_rgba,
        WriteThumbMetadata {
            canonical_uri,
            mtime_sec,
            file_size: None,
            source_dims: None,
        },
    )
}

/// Parses `IHDR` thumbnail dimensions and Freedesktop `tEXt` chunks (`Thumb::MTime`,
/// `Thumb::Image::Width`, `Thumb::Image::Height`) from a PNG byte buffer without heap allocation.
pub fn parse_png_thumbnail_metadata(header: &[u8]) -> Option<PngThumbMetadata> {
    if header.len() < 8 || &header[0..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let mut thumb_w = 0u32;
    let mut thumb_h = 0u32;
    let mut mtime_sec = None;
    let mut full_w_opt = None;
    let mut full_h_opt = None;
    let mut offset = 8;

    while offset + 8 <= header.len() {
        let length = u32::from_be_bytes(header[offset..offset + 4].try_into().ok()?) as usize;
        let chunk_type = &header[offset + 4..offset + 8];
        let data_start = offset + 8;
        let data_end = (data_start + length).min(header.len());

        if chunk_type == b"IEND" || chunk_type == b"IDAT" {
            break;
        }

        if chunk_type == b"IHDR" && data_start + 8 <= header.len() {
            thumb_w = u32::from_be_bytes(header[data_start..data_start + 4].try_into().ok()?);
            thumb_h = u32::from_be_bytes(header[data_start + 4..data_start + 8].try_into().ok()?);
        } else if chunk_type == b"tEXt" && data_start <= header.len() {
            let data = &header[data_start..data_end];
            if let Some(null_pos) = data.iter().position(|&b| b == 0) {
                let keyword = &data[..null_pos];
                let value = &data[null_pos + 1..];
                if keyword == b"Thumb::MTime" {
                    if let Ok(s) = std::str::from_utf8(value) {
                        mtime_sec = s.trim().parse::<i64>().ok();
                    }
                } else if keyword == b"Thumb::Image::Width"
                    && let Ok(s) = std::str::from_utf8(value)
                    && let Ok(w) = s.trim().parse::<u32>()
                    && w > 0
                {
                    full_w_opt = Some(w);
                } else if keyword == b"Thumb::Image::Height"
                    && let Ok(s) = std::str::from_utf8(value)
                    && let Ok(h) = s.trim().parse::<u32>()
                    && h > 0
                {
                    full_h_opt = Some(h);
                }
            }
        }

        offset = match offset.checked_add(12 + length) {
            Some(next) => next,
            None => break,
        };
    }

    if thumb_w == 0 || thumb_h == 0 {
        return None;
    }
    let source_dims = full_w_opt.zip(full_h_opt);
    Some(PngThumbMetadata {
        thumb_w,
        thumb_h,
        mtime_sec,
        source_dims,
    })
}

/// Parses the `Thumb::MTime` timestamp from a PNG header buffer without heap allocation.
#[cfg(test)]
pub fn parse_png_mtime(header: &[u8]) -> Option<i64> {
    parse_png_thumbnail_metadata(header).and_then(|m| m.mtime_sec)
}

/// Probes a thumbnail file to verify its `Thumb::MTime` matches `expected_mtime`.
///
/// Reads at most 2,048 bytes on the stack without reading or decoding the whole image.
#[cfg(test)]
pub fn probe_thumbnail(thumb_path: &Path, expected_mtime: i64) -> Option<PathBuf> {
    let mut file = std::fs::File::open(thumb_path).ok()?;
    let mut buf = [0u8; 2048];
    let n = file.read(&mut buf).ok()?;
    if parse_png_mtime(&buf[..n]) == Some(expected_mtime) {
        Some(thumb_path.to_path_buf())
    } else {
        None
    }
}

/// Atomically writes an encoded PNG thumbnail with full Freedesktop metadata to the thumbnail directory.
pub fn write_freedesktop_thumbnail_with_metadata(
    cache_root: &Path,
    tier: ThumbTier,
    md5_filename: &str,
    thumb_rgba: &image::RgbaImage,
    meta: WriteThumbMetadata<'_>,
) -> Result<PathBuf, String> {
    let target_dir = cache_root.join(tier.dir_name());

    #[cfg(unix)]
    {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        builder.mode(0o700);
        let _ = builder.create(&target_dir);
    }
    #[cfg(not(unix))]
    {
        let _ = std::fs::create_dir_all(&target_dir);
    }

    let encoded_bytes = encode_thumbnail_png_with_metadata(thumb_rgba, meta)?;

    let pid = std::process::id();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp_filename = format!(".tmp.{}_{}_{}", pid, now, md5_filename);
    let tmp_path = target_dir.join(&tmp_filename);

    let mut open_opts = OpenOptions::new();
    open_opts.write(true).create_new(true);
    #[cfg(unix)]
    open_opts.mode(0o600);

    let mut file = open_opts
        .open(&tmp_path)
        .map_err(|e| format!("Failed to create temporary thumbnail file: {}", e))?;

    let write_res = file.write_all(&encoded_bytes).and_then(|_| file.flush());
    drop(file);

    if let Err(e) = write_res {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!("Failed to write thumbnail bytes: {}", e));
    }

    let final_path = target_dir.join(md5_filename);
    if let Err(e) = std::fs::rename(&tmp_path, &final_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!("Failed to commit thumbnail atomically: {}", e));
    }

    Ok(final_path)
}

/// Atomically writes an encoded PNG thumbnail to the Freedesktop thumbnail directory.
#[cfg(test)]
pub fn write_freedesktop_thumbnail(
    cache_root: &Path,
    tier: ThumbTier,
    md5_filename: &str,
    thumb_rgba: &image::RgbaImage,
    canonical_uri: &str,
    mtime_sec: i64,
) -> Result<PathBuf, String> {
    write_freedesktop_thumbnail_with_metadata(
        cache_root,
        tier,
        md5_filename,
        thumb_rgba,
        WriteThumbMetadata {
            canonical_uri,
            mtime_sec,
            file_size: None,
            source_dims: None,
        },
    )
}

/// Returns the default Freedesktop thumbnail cache directory ($XDG_CACHE_HOME/thumbnails or $HOME/.cache/thumbnails).
pub fn default_thumbnail_cache_root() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME")
        && !xdg.is_empty()
    {
        return Some(PathBuf::from(xdg).join("thumbnails"));
    }
    if let Some(home) = std::env::var_os("HOME")
        && !home.is_empty()
    {
        return Some(PathBuf::from(home).join(".cache/thumbnails"));
    }
    None
}

/// Loads a valid cached thumbnail with original source dimensions for `source_path` if present and fresh in `cache_root`,
/// reading and decoding the thumbnail file in a single file open.
pub fn load_cached_thumbnail_with_metadata(
    cache_root: &Path,
    source_path: &Path,
) -> Option<CachedThumbnail> {
    let metadata = std::fs::metadata(source_path).ok()?;
    let mtime = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;

    let (_uri, md5_filename) = cache_key_for_path(source_path)?;

    // Try tiers in priority order: Large (256), Normal (128), XLarge (512), XXLarge (1024)
    let tiers = [
        ThumbTier::Large,
        ThumbTier::Normal,
        ThumbTier::XLarge,
        ThumbTier::XXLarge,
    ];
    for tier in tiers {
        let p = cache_root.join(tier.dir_name()).join(&md5_filename);
        let mut file = match std::fs::File::open(&p) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let file_meta = match file.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let file_len = file_meta.len();
        if !(33..=2 * 1024 * 1024).contains(&file_len) {
            continue;
        }
        let mut png_bytes = Vec::with_capacity(file_len as usize);
        if file.read_to_end(&mut png_bytes).is_err() || png_bytes.len() != file_len as usize {
            continue;
        }
        let meta = match parse_png_thumbnail_metadata(&png_bytes) {
            Some(m) if m.mtime_sec == Some(mtime) => m,
            _ => continue,
        };
        let img = match image::load_from_memory_with_format(&png_bytes, image::ImageFormat::Png) {
            Ok(loaded) => match loaded {
                DynamicImage::ImageRgb8(_) | DynamicImage::ImageRgba8(_) => loaded,
                other => DynamicImage::ImageRgba8(crate::imaging::into_rgba8_fast(other)),
            },
            Err(_) => continue,
        };
        return Some(CachedThumbnail {
            image: img,
            source_dims: meta.source_dims,
        });
    }
    None
}

/// Loads a valid cached thumbnail for `source_path` if present and fresh in `cache_root`.
#[cfg(test)]
pub fn load_cached_thumbnail(cache_root: &Path, source_path: &Path) -> Option<DynamicImage> {
    load_cached_thumbnail_with_metadata(cache_root, source_path).map(|c| c.image)
}

/// Downscales and saves a Freedesktop thumbnail for `source_path` to `cache_root`,
/// recording original source dimensions and file size into PNG `tEXt` metadata.
pub fn save_cached_thumbnail_with_dims(
    cache_root: &Path,
    resizer: &mut fir::Resizer,
    source_path: &Path,
    img: &DynamicImage,
    source_dims: (u32, u32),
) -> Option<PathBuf> {
    if img.width() <= 256 && img.height() <= 256 {
        return None;
    }
    if source_path.starts_with(cache_root) {
        return None;
    }
    let metadata = std::fs::metadata(source_path).ok()?;
    let mtime = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    let file_size = metadata.len();

    let (canonical_uri, md5_filename) = cache_key_for_path(source_path)?;

    let thumb_rgba = downscale_for_thumbnail(resizer, img, ThumbTier::Large).ok()?;
    write_freedesktop_thumbnail_with_metadata(
        cache_root,
        ThumbTier::Large,
        &md5_filename,
        &thumb_rgba,
        WriteThumbMetadata {
            canonical_uri: &canonical_uri,
            mtime_sec: mtime,
            file_size: Some(file_size),
            source_dims: Some(source_dims),
        },
    )
    .ok()
}

/// Downscales and saves a Freedesktop thumbnail for `source_path` to `cache_root`.
#[cfg(test)]
pub fn save_cached_thumbnail(
    cache_root: &Path,
    resizer: &mut fir::Resizer,
    source_path: &Path,
    img: &DynamicImage,
) -> Option<PathBuf> {
    save_cached_thumbnail_with_dims(
        cache_root,
        resizer,
        source_path,
        img,
        (img.width(), img.height()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_md5_rfc1321_vectors() {
        let test_cases = [
            ("", "d41d8cd98f00b204e9800998ecf8427e"),
            ("a", "0cc175b9c0f1b6a831c399e269772661"),
            ("abc", "900150983cd24fb0d6963f7d28e17f72"),
            ("message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
            (
                "abcdefghijklmnopqrstuvwxyz",
                "c3fcd3d76192e4007dfb496cca67e13b",
            ),
        ];

        for (input, expected) in test_cases {
            assert_eq!(
                md5_hex(input.as_bytes()),
                expected,
                "MD5 mismatch for input {:?}",
                input
            );
        }
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "{}_{}_{}",
                name,
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn test_freedesktop_thumbnail_roundtrip_and_mtime_invalidation() {
        let temp_dir = TempDir::new("imv_thumb_test");
        let cache_root = temp_dir.path().join("thumbnails");
        let source_path = temp_dir.path().join("source_photo.png");

        // 1. Create a 600x400 source image
        let src_img = DynamicImage::ImageRgb8(image::RgbImage::new(600, 400));
        src_img.save(&source_path).unwrap();

        // 2. Test downscale_for_thumbnail
        let mut resizer = fir::Resizer::new();
        let thumb_rgba = downscale_for_thumbnail(&mut resizer, &src_img, ThumbTier::Large).unwrap();
        // Preserves 3:2 aspect ratio within 256x256 -> 256x171
        assert_eq!(thumb_rgba.width(), 256);
        assert_eq!(thumb_rgba.height(), 171);

        // 3. Test encode_thumbnail_png & parse_png_mtime
        let (canonical_uri, md5_filename) = cache_key_for_path(&source_path).unwrap();
        assert!(canonical_uri.starts_with("file://"));
        assert!(md5_filename.ends_with(".png"));

        let mtime_sec = 1_700_000_000i64;
        let png_bytes = encode_thumbnail_png(&thumb_rgba, &canonical_uri, mtime_sec).unwrap();
        assert_eq!(parse_png_mtime(&png_bytes), Some(mtime_sec));

        // 4. Test write_freedesktop_thumbnail
        let written_path = write_freedesktop_thumbnail(
            &cache_root,
            ThumbTier::Large,
            &md5_filename,
            &thumb_rgba,
            &canonical_uri,
            mtime_sec,
        )
        .unwrap();
        assert!(written_path.exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::metadata(&written_path).unwrap().permissions();
            assert_eq!(perms.mode() & 0o777, 0o600);
        }

        // 5. Test probe_thumbnail: matching mtime vs mismatched mtime
        assert_eq!(
            probe_thumbnail(&written_path, mtime_sec),
            Some(written_path.clone())
        );
        assert_eq!(probe_thumbnail(&written_path, mtime_sec + 1), None);

        // 6. Test save_cached_thumbnail & load_cached_thumbnail
        let saved =
            save_cached_thumbnail(&cache_root, &mut resizer, &source_path, &src_img).unwrap();
        assert!(saved.exists());

        let loaded = load_cached_thumbnail(&cache_root, &source_path);
        assert!(loaded.is_some());
        let loaded_img = loaded.unwrap();
        assert_eq!(loaded_img.width(), 256);
        assert_eq!(loaded_img.height(), 171);
    }

    #[test]
    fn test_freedesktop_thumbnail_metadata_source_dims_and_single_open_load() {
        let temp_dir = TempDir::new("imv_thumb_meta_test");
        let cache_root = temp_dir.path().join("thumbnails");
        let source_path = temp_dir.path().join("mock_raw.cr2");

        // 1. Create a 600x400 mock source image on disk
        let src_img = DynamicImage::ImageRgb8(image::RgbImage::new(600, 400));
        src_img
            .save_with_format(&source_path, image::ImageFormat::Png)
            .unwrap();

        let source_mtime_sec = std::fs::metadata(&source_path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        // 2. Save cached thumbnail with custom source_dims (e.g. true RAW sensor dims 6000x4000)
        let mut resizer = fir::Resizer::new();
        let saved_path = save_cached_thumbnail_with_dims(
            &cache_root,
            &mut resizer,
            &source_path,
            &src_img,
            (6000, 4000),
        )
        .expect("save_cached_thumbnail_with_dims should succeed");
        assert!(saved_path.exists());

        // 3. Inspect raw bytes of saved PNG thumbnail
        let written_bytes = std::fs::read(&saved_path).unwrap();
        assert!(
            written_bytes
                .windows(b"Thumb::Size".len())
                .any(|w| w == b"Thumb::Size")
        );
        assert!(
            written_bytes
                .windows(b"Thumb::Image::Width".len())
                .any(|w| w == b"Thumb::Image::Width")
        );
        assert!(
            written_bytes
                .windows(b"Thumb::Image::Height".len())
                .any(|w| w == b"Thumb::Image::Height")
        );
        assert!(
            written_bytes
                .windows(b"Software\0imv-tui".len())
                .any(|w| w == b"Software\0imv-tui")
        );

        let meta = parse_png_thumbnail_metadata(&written_bytes)
            .expect("parse_png_thumbnail_metadata should succeed");
        assert_eq!(meta.thumb_w, 256);
        assert_eq!(meta.thumb_h, 171);
        assert_eq!(meta.mtime_sec, Some(source_mtime_sec));
        assert_eq!(meta.source_dims, Some((6000, 4000)));

        // 4. Test load_cached_thumbnail_with_metadata: loads decoded image and retains source_dims
        let cached = load_cached_thumbnail_with_metadata(&cache_root, &source_path)
            .expect("load_cached_thumbnail_with_metadata should succeed");
        assert_eq!(cached.image.width(), 256);
        assert_eq!(cached.image.height(), 171);
        assert_eq!(cached.source_dims, Some((6000, 4000)));

        // 5. Invalidate source mtime and ensure cache miss
        #[cfg(unix)]
        {
            let new_time = libc::timespec {
                tv_sec: source_mtime_sec + 10,
                tv_nsec: 0,
            };
            let times = [new_time, new_time];
            let path_c = std::ffi::CString::new(source_path.to_str().unwrap()).unwrap();
            unsafe {
                assert_eq!(
                    libc::utimensat(libc::AT_FDCWD, path_c.as_ptr(), times.as_ptr(), 0),
                    0
                );
            }
            assert!(load_cached_thumbnail_with_metadata(&cache_root, &source_path).is_none());
        }
    }

    #[test]
    fn test_downscale_for_thumbnail_rgb8_direct_and_no_alpha_buffer() {
        let mut resizer = fir::Resizer::new();

        // 1. Same size / smaller than target tier: identity pass-through unpacked to RGBA
        let small_rgb = DynamicImage::ImageRgb8(image::RgbImage::new(100, 80));
        let small_thumb =
            downscale_for_thumbnail(&mut resizer, &small_rgb, ThumbTier::Normal).unwrap();
        assert_eq!(small_thumb.width(), 100);
        assert_eq!(small_thumb.height(), 80);
        assert_eq!(small_thumb.get_pixel(0, 0)[3], 255);
        assert_eq!(small_thumb.get_pixel(99, 79)[3], 255);
        // Identity path does not call resizer at all
        assert_eq!(resizer.size_of_internal_buffers(), 0);

        // 2. Downscaling large RGB8 image: preserves aspect ratio, alpha is 255, and no alpha_buffer allocated
        let large_rgb = DynamicImage::ImageRgb8(image::RgbImage::new(600, 400));
        let large_thumb =
            downscale_for_thumbnail(&mut resizer, &large_rgb, ThumbTier::Large).unwrap();
        assert_eq!(large_thumb.width(), 256);
        assert_eq!(large_thumb.height(), 171);
        assert_eq!(large_thumb.get_pixel(0, 0)[3], 255);
        assert_eq!(large_thumb.get_pixel(255, 170)[3], 255);

        let rgb_buf_size = resizer.size_of_internal_buffers();

        // 3. Contrast with ImageRgba8 of same dimensions: RGBA allocates a large alpha_buffer (600 * 400 * 4 = 960,000 bytes)
        resizer.reset_internal_buffers();
        let large_rgba = DynamicImage::ImageRgba8(image::RgbaImage::new(600, 400));
        let rgba_thumb =
            downscale_for_thumbnail(&mut resizer, &large_rgba, ThumbTier::Large).unwrap();
        assert_eq!(rgba_thumb.width(), 256);
        assert_eq!(rgba_thumb.height(), 171);

        let rgba_buf_size = resizer.size_of_internal_buffers();
        assert!(rgba_buf_size >= 600 * 400 * 4);
        assert!(rgb_buf_size < 500_000);
        assert!(rgba_buf_size > rgb_buf_size + 900_000);
    }
}
