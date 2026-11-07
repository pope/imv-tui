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
    let src_rgba = crate::imaging::to_rgba8_fast(src_img);

    if target_w == src_w && target_h == src_h {
        return Ok(src_rgba.into_owned());
    }

    let mut dst_image = fir::images::Image::new(target_w, target_h, fir::PixelType::U8x4);

    let mut options = fir::ResizeOptions::new();
    options.algorithm = fir::ResizeAlg::Convolution(fir::FilterType::Mitchell);
    options.mul_div_alpha = true;

    resizer
        .resize(src_rgba.as_ref(), &mut dst_image, Some(&options))
        .map_err(|e| format!("Thumbnail resize failed: {}", e))?;

    let dst_raw = dst_image.into_vec();
    image::RgbaImage::from_raw(target_w, target_h, dst_raw)
        .ok_or_else(|| "Failed to construct RgbaImage from resized buffer".to_string())
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

/// Encodes an RGBA8 thumbnail image to PNG bytes with injected Freedesktop `tEXt` chunks.
///
/// Guaranteed to position `Thumb::MTime` within the first 2,048 bytes of the file.
pub fn encode_thumbnail_png(
    thumb_rgba: &image::RgbaImage,
    canonical_uri: &str,
    mtime_sec: i64,
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

    let chunk_uri = make_text_chunk("Thumb::URI", canonical_uri);
    let chunk_mtime = make_text_chunk("Thumb::MTime", &mtime_sec.to_string());
    let chunk_sw = make_text_chunk("Software", "imv-tui");

    let total_len = png_bytes.len() + chunk_uri.len() + chunk_mtime.len() + chunk_sw.len();
    let mut result = Vec::with_capacity(total_len);
    // Standard PNG header + IHDR chunk is exactly 33 bytes
    result.extend_from_slice(&png_bytes[..33]);
    result.extend_from_slice(&chunk_uri);
    result.extend_from_slice(&chunk_mtime);
    result.extend_from_slice(&chunk_sw);
    result.extend_from_slice(&png_bytes[33..]);

    Ok(result)
}

/// Parses the `Thumb::MTime` timestamp from a PNG header buffer without heap allocation.
pub fn parse_png_mtime(header: &[u8]) -> Option<i64> {
    if header.len() < 8 || &header[0..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let mut offset = 8;
    while offset + 8 <= header.len() {
        let length = u32::from_be_bytes(header[offset..offset + 4].try_into().ok()?) as usize;
        let chunk_type = &header[offset + 4..offset + 8];
        let data_start = offset + 8;
        let data_end = (data_start + length).min(header.len());

        if chunk_type == b"tEXt" && data_start <= header.len() {
            let data = &header[data_start..data_end];
            if let Some(null_pos) = data.iter().position(|&b| b == 0) {
                let keyword = &data[..null_pos];
                if keyword == b"Thumb::MTime" {
                    let value_str = std::str::from_utf8(&data[null_pos + 1..]).ok()?;
                    return value_str.trim().parse::<i64>().ok();
                }
            }
        } else if chunk_type == b"IDAT" {
            break;
        }

        offset = match offset.checked_add(12 + length) {
            Some(next) => next,
            None => break,
        };
    }
    None
}

/// Probes a thumbnail file to verify its `Thumb::MTime` matches `expected_mtime`.
///
/// Reads at most 2,048 bytes on the stack without reading or decoding the whole image.
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

/// Atomically writes an encoded PNG thumbnail to the Freedesktop thumbnail directory.
pub fn write_freedesktop_thumbnail(
    cache_root: &Path,
    tier: ThumbTier,
    md5_filename: &str,
    thumb_rgba: &image::RgbaImage,
    canonical_uri: &str,
    mtime_sec: i64,
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

    let encoded_bytes = encode_thumbnail_png(thumb_rgba, canonical_uri, mtime_sec)?;

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

/// Loads a valid cached thumbnail for `source_path` if present and fresh in `cache_root`.
pub fn load_cached_thumbnail(cache_root: &Path, source_path: &Path) -> Option<DynamicImage> {
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
        if let Some(valid_path) = probe_thumbnail(&p, mtime)
            && let Ok(img) = image::open(&valid_path)
        {
            return Some(img);
        }
    }
    None
}

/// Downscales and saves a Freedesktop thumbnail for `source_path` to `cache_root`.
pub fn save_cached_thumbnail(
    cache_root: &Path,
    resizer: &mut fir::Resizer,
    source_path: &Path,
    img: &DynamicImage,
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

    let (canonical_uri, md5_filename) = cache_key_for_path(source_path)?;

    let thumb_rgba = downscale_for_thumbnail(resizer, img, ThumbTier::Large).ok()?;
    write_freedesktop_thumbnail(
        cache_root,
        ThumbTier::Large,
        &md5_filename,
        &thumb_rgba,
        &canonical_uri,
        mtime,
    )
    .ok()
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
}
