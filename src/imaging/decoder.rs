//! High-level image decoding logic, file type guessing, and source scanning.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::imaging::DecodedImage;
use crate::imaging::ImageSource;
use crate::imaging::raw::*;
use image::{DynamicImage, ImageDecoder};

/// Cooperative cancellation token checked at multi-step I/O and decode checkpoints.
#[derive(Clone, Copy, Debug, Default)]
pub struct CancellationToken<'a> {
    cancelled: Option<&'a AtomicBool>,
    sequence: Option<(usize, &'a AtomicUsize)>,
}

impl<'a> CancellationToken<'a> {
    /// Creates a dummy token that never cancels and performs zero allocations.
    #[cfg(test)]
    #[inline]
    pub const fn none() -> Self {
        Self {
            cancelled: None,
            sequence: None,
        }
    }

    /// Creates a new active cancellation token backed by a borrowed boolean flag.
    #[cfg(test)]
    #[inline]
    pub const fn from_flag(flag: &'a AtomicBool) -> Self {
        Self {
            cancelled: Some(flag),
            sequence: None,
        }
    }

    /// Creates a token that cancels automatically when `active_seq` no longer matches `seq`.
    #[inline]
    pub const fn with_sequence(seq: usize, active_seq: &'a AtomicUsize) -> Self {
        Self {
            cancelled: None,
            sequence: Some((seq, active_seq)),
        }
    }

    /// Signals cancellation on the underlying boolean flag, if present.
    #[cfg(test)]
    #[inline]
    pub fn cancel(&self) {
        if let Some(c) = self.cancelled {
            c.store(true, Ordering::Release);
        }
    }

    /// Returns `true` if cancellation has been signaled or the sequence is obsolete.
    #[inline]
    pub fn is_cancelled(&self) -> bool {
        if let Some(c) = self.cancelled
            && c.load(Ordering::Acquire)
        {
            return true;
        }
        if let Some((seq, active)) = self.sequence
            && active.load(Ordering::Acquire) != seq
        {
            return true;
        }
        false
    }
}

/// Guessed type based on magic bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuessType {
    /// Valid image formats.
    Image,
    /// Zip file / comic book archive format.
    Zip,
}

/// Uses file magic bytes headers to guess the file format type.
pub fn guess_file_type(path: &Path) -> Option<GuessType> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut header = [0u8; 16];
    use std::io::Read;
    let n = file.read(&mut header).ok()?;
    let bytes = &header[..n];

    if bytes.len() >= 2 && bytes[0] == 0x50 && bytes[1] == 0x4B {
        return Some(GuessType::Zip);
    }

    if image::guess_format(bytes).is_ok() {
        return Some(GuessType::Image);
    }

    None
}

/// Returns true if the file path has a supported image extension.
pub fn is_supported_image_ext(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| {
            ext.eq_ignore_ascii_case("png")
                || ext.eq_ignore_ascii_case("jpg")
                || ext.eq_ignore_ascii_case("jpeg")
                || ext.eq_ignore_ascii_case("gif")
                || ext.eq_ignore_ascii_case("webp")
                || ext.eq_ignore_ascii_case("bmp")
                || ext.eq_ignore_ascii_case("tiff")
                || ext.eq_ignore_ascii_case("ico")
                || is_raw_ext(ext)
        })
        .unwrap_or(false)
}

/// Checks if a file path points to a supported image.
pub fn is_image_file(path: &Path, check_magic: bool) -> bool {
    if !path.is_file() {
        return false;
    }
    if check_magic {
        matches!(guess_file_type(path), Some(GuessType::Image))
    } else {
        is_supported_image_ext(path)
    }
}

/// Helper to determine if a file is a comic book archive (CBZ) or generic ZIP.
pub fn is_cbz_or_zip(path: &Path, check_magic: bool) -> bool {
    if !path.is_file() {
        return false;
    }
    if check_magic {
        matches!(guess_file_type(path), Some(GuessType::Zip))
    } else {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("cbz") || ext.eq_ignore_ascii_case("zip"))
            .unwrap_or(false)
    }
}

/// Lists all sorted image files inside a CBZ or ZIP archive.
pub fn list_cbz_pages(zip_path: &Path) -> Result<Vec<String>, String> {
    let file = std::fs::File::open(zip_path)
        .map_err(|e| format!("Failed to open zip file {}: {}", zip_path.display(), e))?;
    let reader = std::io::BufReader::with_capacity(64 * 1024, file);
    let archive = zip::ZipArchive::new(reader)
        .map_err(|e| format!("Failed to read zip archive {}: {}", zip_path.display(), e))?;

    let mut pages: Vec<String> = archive
        .file_names()
        .filter_map(Result::ok)
        .filter(|name| !name.ends_with('/') && is_supported_image_ext(Path::new(name.as_ref())))
        .map(|name| name.to_string())
        .collect();

    pages.sort_by(|a, b| {
        a.bytes()
            .map(|c| c.to_ascii_lowercase())
            .cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
            .then_with(|| a.cmp(b))
    });
    Ok(pages)
}

/// Recursively traverses and collects all valid ImageSource items from a list of paths.
pub fn collect_sources(paths: &[PathBuf], check_magic: bool) -> Result<Vec<ImageSource>, String> {
    let mut sources = Vec::with_capacity(paths.len());
    for path in paths {
        if is_cbz_or_zip(path, check_magic) {
            match list_cbz_pages(path) {
                Ok(pages) => {
                    for page in pages {
                        sources.push(ImageSource::Cbz {
                            zip_path: path.clone(),
                            file_in_zip: page,
                        });
                    }
                }
                Err(e) => {
                    eprintln!(
                        "Warning: Skipping corrupt zip archive {}: {}",
                        path.display(),
                        e
                    );
                }
            }
        } else if is_image_file(path, check_magic) {
            sources.push(ImageSource::Local(path.clone()));
        }
    }
    Ok(sources)
}

/// Scans the directory surrounding the initial path, returns sorted paths of
/// supported images along with the index pointing to the initial file.
pub fn scan_directory(
    initial_path: &Path,
    check_magic: bool,
    recursive: bool,
) -> Result<(Vec<PathBuf>, usize), String> {
    let (dir, file_name) = if initial_path.is_file() {
        let parent = initial_path.parent().unwrap_or_else(|| Path::new("."));
        let name = initial_path.file_name().map(|n| n.to_os_string());
        (parent.to_path_buf(), name)
    } else if initial_path.is_dir() {
        (initial_path.to_path_buf(), None)
    } else {
        let parent = Path::new(".");
        if parent.join(initial_path).is_file() {
            (
                parent.to_path_buf(),
                Some(initial_path.as_os_str().to_os_string()),
            )
        } else {
            return Err(format!("Path does not exist: {}", initial_path.display()));
        }
    };

    let mut images = Vec::new();
    let mut visited_files = std::collections::HashSet::new();

    if recursive {
        let mut visited = std::collections::HashSet::new();
        let mut stack = vec![(dir.clone(), 0)]; // Store path and current depth
        let max_depth = 32;

        while let Some((current_dir, depth)) = stack.pop() {
            if depth > max_depth {
                continue;
            }
            if let Ok(canonical) = current_dir.canonicalize() {
                if !visited.insert(canonical) {
                    continue; // Skip circular symlink
                }
            } else {
                continue;
            }

            if let Ok(entries) = fs::read_dir(&current_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push((path, depth + 1));
                    } else if is_image_file(&path, check_magic) {
                        if let Ok(canonical) = path.canonicalize() {
                            if visited_files.insert(canonical) {
                                images.push(path);
                            }
                        } else {
                            images.push(path);
                        }
                    }
                }
            }
        }
    } else if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if is_image_file(&path, check_magic) {
                if let Ok(canonical) = path.canonicalize() {
                    if visited_files.insert(canonical) {
                        images.push(path);
                    }
                } else {
                    images.push(path);
                }
            }
        }
    }

    images.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

    let index = if let Some(ref target_name) = file_name {
        images
            .iter()
            .position(|path| {
                if let (Ok(c1), Ok(c2)) = (path.canonicalize(), initial_path.canonicalize()) {
                    c1 == c2
                } else {
                    path.file_name().map(|n| n == target_name).unwrap_or(false)
                }
            })
            .unwrap_or_else(|| {
                if initial_path.exists() {
                    images.push(initial_path.to_path_buf());
                    images.len() - 1
                } else {
                    0
                }
            })
    } else {
        0
    };

    Ok((images, index))
}

/// Decodes an image from already loaded memory bytes with cooperative cancellation support.
#[cfg(test)]
pub fn decode_image_bytes_cancellable(
    bytes: &[u8],
    source: &ImageSource,
    cancel: &CancellationToken<'_>,
) -> Result<DecodedImage, String> {
    decode_image_bytes_with_header_cancellable(bytes, source, None, cancel)
}

/// Decodes an image from memory bytes with cooperative cancellation support, optionally reusing
/// pre-probed `header_bytes` for RAW container parsing to avoid duplicate file I/O.
pub fn decode_image_bytes_with_header_cancellable(
    bytes: &[u8],
    source: &ImageSource,
    header_bytes: Option<&[u8]>,
    cancel: &CancellationToken<'_>,
) -> Result<DecodedImage, String> {
    if cancel.is_cancelled() {
        return Err("Operation cancelled".to_string());
    }

    let file_size = match source {
        ImageSource::Local(path) => std::fs::metadata(path)
            .map(|m| m.len())
            .unwrap_or(bytes.len() as u64),
        _ => bytes.len() as u64,
    };
    let format = image::guess_format(bytes).ok();
    let display_name = source.display_name();

    // Intercept RAW/TIFF files to extract and decode their embedded JPEG preview/thumbnail
    let is_raw_extension = source.is_raw();

    let mut raw_dimensions = None;
    let mut raw_container_orientation = None;
    if is_raw_extension {
        let partial_bytes_owned;
        let partial_bytes: Option<&[u8]> = match header_bytes {
            Some(hb) => Some(hb),
            None => {
                partial_bytes_owned = read_source_bytes_limited(source, RAW_EXIF_PROBE_LIMIT).ok();
                partial_bytes_owned.as_deref()
            }
        };

        // Inspect raw container format for dimensions and orientation
        if let Some(partial_bytes) = partial_bytes {
            if partial_bytes.starts_with(b"FUJIFILM")
                && let Some(meta_offset) = partial_bytes
                    .get(92..96)
                    .and_then(|b| b.try_into().ok())
                    .map(u32::from_be_bytes)
                    .map(|x| x as u64)
                && let Some(meta_length) = partial_bytes
                    .get(96..100)
                    .and_then(|b| b.try_into().ok())
                    .map(u32::from_be_bytes)
                    .map(|x| x as u64)
                && let Ok(meta_bytes) = read_source_range(source, meta_offset, meta_length)
                && let Some((w, h)) = get_fuji_raf_dimensions(&meta_bytes)
            {
                // Get orientation from the JPEG preview (the bytes variable contains the preview JPEG!)
                let orientation = get_exif_orientation(bytes);
                let swaps = matches!(
                    orientation,
                    image::metadata::Orientation::Rotate90
                        | image::metadata::Orientation::Rotate270
                        | image::metadata::Orientation::Rotate90FlipH
                        | image::metadata::Orientation::Rotate270FlipH
                );
                if swaps {
                    raw_dimensions = Some((h, w));
                } else {
                    raw_dimensions = Some((w, h));
                }
            } else if let Some(exif) = parse_tolerant_exif(partial_bytes) {
                // If not Fuji RAF, try standard TIFF EXIF container check
                let orientation = get_orientation_from_exif(&exif);
                raw_container_orientation = Some(orientation);
                if let Some((w, h)) = get_dimensions_from_exif(&exif) {
                    let swaps = matches!(
                        orientation,
                        image::metadata::Orientation::Rotate90
                            | image::metadata::Orientation::Rotate270
                            | image::metadata::Orientation::Rotate90FlipH
                            | image::metadata::Orientation::Rotate270FlipH
                    );
                    if swaps {
                        raw_dimensions = Some((h, w));
                    } else {
                        raw_dimensions = Some((w, h));
                    }
                }
            }
        }
    }

    if is_raw_extension
        && raw_dimensions.is_none()
        && let Some(exif) = parse_tolerant_exif(bytes)
        && let Some((w, h)) = get_dimensions_from_exif(&exif)
    {
        let orientation = get_orientation_from_exif(&exif);
        let swaps = matches!(
            orientation,
            image::metadata::Orientation::Rotate90
                | image::metadata::Orientation::Rotate270
                | image::metadata::Orientation::Rotate90FlipH
                | image::metadata::Orientation::Rotate270FlipH
        );
        if swaps {
            raw_dimensions = Some((h, w));
        } else {
            raw_dimensions = Some((w, h));
        }
    }

    if format != Some(image::ImageFormat::Jpeg)
        && (format == Some(image::ImageFormat::Tiff) || is_raw_extension)
        && let Some(jpeg_bytes) = extract_jpeg_preview(bytes)
    {
        // Recurse using the extracted JPEG bytes
        if let Ok(mut decoded) =
            decode_image_bytes_with_header_cancellable(&jpeg_bytes, source, header_bytes, cancel)
        {
            decoded.format = Some(image::ImageFormat::Jpeg);
            decoded.disk_size = file_size;
            if raw_dimensions.is_some() {
                decoded.raw_dimensions = raw_dimensions;
            }
            return Ok(decoded);
        }
    }

    if let Some(image::ImageFormat::Jpeg) = format {
        let options = zune_jpeg::zune_core::options::DecoderOptions::default()
            .jpeg_set_out_colorspace(zune_jpeg::zune_core::colorspace::ColorSpace::RGB);
        let cursor = zune_jpeg::zune_core::bytestream::ZCursor::new(bytes);
        let mut decoder = zune_jpeg::JpegDecoder::new_with_options(cursor, options);

        // Checkpoint 2 before JPEG pixel decoding
        if cancel.is_cancelled() {
            return Err("Operation cancelled".to_string());
        }

        if let Ok(pixels) = decoder.decode()
            && let Some(info) = decoder.info()
            && let Some(rgb_img) =
                image::RgbImage::from_raw(info.width as u32, info.height as u32, pixels)
        {
            let mut orientation = get_exif_orientation(bytes);
            if orientation == image::metadata::Orientation::NoTransforms && is_raw_extension {
                orientation =
                    raw_container_orientation.unwrap_or(image::metadata::Orientation::NoTransforms);
            }

            let mut img = image::DynamicImage::ImageRgb8(rgb_img);

            // Checkpoint 3 before JPEG orientation transforms
            if orientation != image::metadata::Orientation::NoTransforms {
                if cancel.is_cancelled() {
                    return Err("Operation cancelled".to_string());
                }
                img.apply_orientation(orientation);
            }

            let w = img.width();
            let h = img.height();
            let final_raw_dims = raw_dimensions.unwrap_or((w, h));
            return Ok(DecodedImage {
                image: img,
                width: w,
                height: h,
                format: Some(image::ImageFormat::Jpeg),
                disk_size: file_size,
                raw_dimensions: Some(final_raw_dims),
            });
        }
    }

    let cursor = std::io::Cursor::new(bytes);
    let reader = image::ImageReader::new(cursor)
        .with_guessed_format()
        .map_err(|e| {
            format!(
                "Failed to parse image from memory:\n{}\nError: {}",
                display_name, e
            )
        })?;

    let mut decoder = reader
        .into_decoder()
        .map_err(|e| format!("Failed to read metadata:\n{}\n\nError: {}", display_name, e))?;

    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);

    // Checkpoint 2 before image-rs decoding
    if cancel.is_cancelled() {
        return Err("Operation cancelled".to_string());
    }

    let mut img = image::DynamicImage::from_decoder(decoder)
        .map_err(|e| format!("Failed to decode image:\n{}\n\nError: {}", display_name, e))?;

    // Checkpoint 3 before orientation transforms
    if orientation != image::metadata::Orientation::NoTransforms {
        if cancel.is_cancelled() {
            return Err("Operation cancelled".to_string());
        }
        img.apply_orientation(orientation);
    }

    let img = match img {
        image::DynamicImage::ImageRgb8(_) | image::DynamicImage::ImageRgba8(_) => img,
        other => image::DynamicImage::ImageRgba8(into_rgba8_fast(other)),
    };
    let w = img.width();
    let h = img.height();
    let final_raw_dims = raw_dimensions.unwrap_or((w, h));
    Ok(DecodedImage {
        image: img,
        width: w,
        height: h,
        format,
        disk_size: file_size,
        raw_dimensions: Some(final_raw_dims),
    })
}

/// Unpacks packed 24-bit RGB (`[R, G, B]`) bytes into 32-bit RGBA (`[R, G, B, 255]`) bytes
/// using 4-pixel (12-byte -> 16-byte) unrolled block unpacking for SIMD auto-vectorization.
pub fn unpack_rgb24_to_rgba32(rgb: &[u8]) -> Vec<u8> {
    let num_pixels = rgb.len() / 3;
    let mut rgba = vec![0u8; num_pixels * 4];
    let (in_chunks, in_rem) = rgb[..num_pixels * 3].as_chunks::<12>();
    let (out_chunks, out_rem) = rgba.as_chunks_mut::<16>();

    for (src, dst) in in_chunks.iter().zip(out_chunks.iter_mut()) {
        *dst = [
            src[0], src[1], src[2], 0xFF, src[3], src[4], src[5], 0xFF, src[6], src[7], src[8],
            0xFF, src[9], src[10], src[11], 0xFF,
        ];
    }

    for (src, dst) in in_rem
        .as_chunks::<3>()
        .0
        .iter()
        .zip(out_rem.as_chunks_mut::<4>().0.iter_mut())
    {
        *dst = [src[0], src[1], src[2], 0xFF];
    }

    rgba
}

/// Converts a `DynamicImage` into an `RgbaImage` by consuming it, utilizing SIMD-vectorized
/// unpacking when the source is `DynamicImage::ImageRgb8` to avoid per-pixel allocations.
pub fn into_rgba8_fast(img: DynamicImage) -> image::RgbaImage {
    match img {
        DynamicImage::ImageRgba8(rgba) => rgba,
        DynamicImage::ImageRgb8(rgb) => {
            let (w, h) = (rgb.width(), rgb.height());
            let raw = unpack_rgb24_to_rgba32(rgb.as_raw());
            image::RgbaImage::from_raw(w, h, raw).expect("valid RGBA dimensions")
        }
        other => other.into_rgba8(),
    }
}

/// Converts a `DynamicImage` reference into a `Cow<RgbaImage>`, borrowing zero-copy when the image
/// is already `DynamicImage::ImageRgba8` and using SIMD-vectorized unpacking for `DynamicImage::ImageRgb8`.
pub fn to_rgba8_fast(img: &DynamicImage) -> std::borrow::Cow<'_, image::RgbaImage> {
    match img {
        DynamicImage::ImageRgba8(rgba) => std::borrow::Cow::Borrowed(rgba),
        DynamicImage::ImageRgb8(rgb) => {
            let (w, h) = (rgb.width(), rgb.height());
            let raw = unpack_rgb24_to_rgba32(rgb.as_raw());
            std::borrow::Cow::Owned(
                image::RgbaImage::from_raw(w, h, raw).expect("valid RGBA dimensions"),
            )
        }
        other => std::borrow::Cow::Owned(other.to_rgba8()),
    }
}

/// Decodes an image from already loaded memory bytes.
#[cfg(test)]
pub fn decode_image_bytes(bytes: &[u8], source: &ImageSource) -> Result<DecodedImage, String> {
    decode_image_bytes_cancellable(bytes, source, &CancellationToken::none())
}

/// Decodes an image from local paths or comic book archives.
/// Employs zune-jpeg for extremely fast decoding of JPEGs.
#[cfg(test)]
pub fn decode_image_source(source: ImageSource) -> Result<DecodedImage, String> {
    decode_image_source_cancellable(source, &CancellationToken::none())
}

/// Decodes an image from local paths or comic book archives with cooperative cancellation support.
#[cfg(test)]
pub fn decode_image_source_cancellable(
    source: ImageSource,
    cancel: &CancellationToken<'_>,
) -> Result<DecodedImage, String> {
    if cancel.is_cancelled() {
        return Err("Operation cancelled".to_string());
    }
    let bytes = read_source_bytes(&source)?;
    decode_image_bytes_cancellable(&bytes, &source, cancel)
}

/// Decodes embedded JPEG thumbnail bytes using `zune_jpeg` into `DynamicImage::ImageRgb8`,
/// falling back to `image::load_from_memory` for non-standard/unusual formats.
fn decode_jpeg_thumbnail_bytes(thumb_bytes: &[u8]) -> Option<DynamicImage> {
    let options = zune_jpeg::zune_core::options::DecoderOptions::default()
        .jpeg_set_out_colorspace(zune_jpeg::zune_core::colorspace::ColorSpace::RGB);
    let cursor = zune_jpeg::zune_core::bytestream::ZCursor::new(thumb_bytes);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(cursor, options);
    if let Ok(pixels) = decoder.decode()
        && let Some(info) = decoder.info()
        && let Some(rgb_img) =
            image::RgbImage::from_raw(info.width as u32, info.height as u32, pixels)
    {
        Some(DynamicImage::ImageRgb8(rgb_img))
    } else {
        image::load_from_memory(thumb_bytes)
            .ok()
            .map(|img| match img {
                DynamicImage::ImageRgb8(_) | DynamicImage::ImageRgba8(_) => img,
                other => DynamicImage::ImageRgba8(into_rgba8_fast(other)),
            })
    }
}

/// Reads the image dimensions extremely quickly from header headers, and decodes/scales
/// its embedded EXIF thumbnail to serve as a fast low-res placeholder for large cold loads.
pub fn decode_thumbnail_and_dimensions(bytes: &[u8]) -> Option<(DynamicImage, u32, u32)> {
    // Check if this is a TIFF/RAW container
    let is_tiff = is_tiff_header(bytes);

    if is_tiff {
        let exif = parse_tolerant_exif(bytes)?;

        let thumb_bytes = extract_jpeg_thumbnail(bytes)?;
        let mut thumb_img = decode_jpeg_thumbnail_bytes(&thumb_bytes)?;

        let orientation = get_orientation_from_exif(&exif);

        thumb_img.apply_orientation(orientation);

        let (mut raw_w, mut raw_h) =
            get_dimensions_from_exif(&exif).unwrap_or((thumb_img.width(), thumb_img.height()));

        let swaps = matches!(
            orientation,
            image::metadata::Orientation::Rotate90
                | image::metadata::Orientation::Rotate270
                | image::metadata::Orientation::Rotate90FlipH
                | image::metadata::Orientation::Rotate270FlipH
        );
        if swaps {
            std::mem::swap(&mut raw_w, &mut raw_h);
        }

        // Crop out any black padding/letterboxing in the thumbnail frame
        let ar_main = raw_w as f64 / raw_h as f64;
        let thumb_w = thumb_img.width();
        let thumb_h = thumb_img.height();
        let ar_thumb = thumb_w as f64 / thumb_h as f64;

        let cropped_thumb = if (ar_main - ar_thumb).abs() > 0.01 {
            if ar_main > ar_thumb {
                let content_h = (thumb_w as f64 / ar_main).round() as u32;
                let padding_y = (thumb_h.saturating_sub(content_h)) / 2;
                let content_h = content_h.min(thumb_h.saturating_sub(padding_y));
                if content_h > 0 {
                    thumb_img.crop_imm(0, padding_y, thumb_w, content_h)
                } else {
                    thumb_img
                }
            } else {
                let content_w = (thumb_h as f64 * ar_main).round() as u32;
                let padding_x = (thumb_w.saturating_sub(content_w)) / 2;
                let content_w = content_w.min(thumb_w.saturating_sub(padding_x));
                if content_w > 0 {
                    thumb_img.crop_imm(padding_x, 0, content_w, thumb_h)
                } else {
                    thumb_img
                }
            }
        } else {
            thumb_img
        };

        let final_thumb = match cropped_thumb {
            DynamicImage::ImageRgb8(_) | DynamicImage::ImageRgba8(_) => cropped_thumb,
            other => DynamicImage::ImageRgba8(into_rgba8_fast(other)),
        };

        return Some((final_thumb, raw_w, raw_h));
    }

    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut decoder = reader.into_decoder().ok()?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let (raw_w, raw_h) = decoder.dimensions();

    let thumb_bytes = extract_jpeg_thumbnail(bytes)?;
    let mut thumb_img = decode_jpeg_thumbnail_bytes(&thumb_bytes)?;

    // Apply the exact same orientation transforms to the thumbnail
    thumb_img.apply_orientation(orientation);

    // Swap dimensions if orientation rotates the image 90 or 270 degrees
    let swaps = matches!(
        orientation,
        image::metadata::Orientation::Rotate90
            | image::metadata::Orientation::Rotate270
            | image::metadata::Orientation::Rotate90FlipH
            | image::metadata::Orientation::Rotate270FlipH
    );

    let (mut oriented_raw_w, mut oriented_raw_h) = (raw_w, raw_h);
    if swaps {
        std::mem::swap(&mut oriented_raw_w, &mut oriented_raw_h);
    }

    // Crop out any black padding/letterboxing in the thumbnail frame
    let ar_main = oriented_raw_w as f64 / oriented_raw_h as f64;
    let thumb_w = thumb_img.width();
    let thumb_h = thumb_img.height();
    let ar_thumb = thumb_w as f64 / thumb_h as f64;

    let cropped_thumb = if (ar_main - ar_thumb).abs() > 0.01 {
        if ar_main > ar_thumb {
            let content_h = (thumb_w as f64 / ar_main).round() as u32;
            let padding_y = (thumb_h.saturating_sub(content_h)) / 2;
            let content_h = content_h.min(thumb_h.saturating_sub(padding_y));
            if content_h > 0 {
                thumb_img.crop_imm(0, padding_y, thumb_w, content_h)
            } else {
                thumb_img
            }
        } else {
            let content_w = (thumb_h as f64 * ar_main).round() as u32;
            let padding_x = (thumb_w.saturating_sub(content_w)) / 2;
            let content_w = content_w.min(thumb_w.saturating_sub(padding_x));
            if content_w > 0 {
                thumb_img.crop_imm(padding_x, 0, content_w, thumb_h)
            } else {
                thumb_img
            }
        }
    } else {
        thumb_img
    };

    let final_thumb = match cropped_thumb {
        DynamicImage::ImageRgb8(_) | DynamicImage::ImageRgba8(_) => cropped_thumb,
        other => DynamicImage::ImageRgba8(into_rgba8_fast(other)),
    };

    Some((final_thumb, oriented_raw_w, oriented_raw_h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_list_cbz_pages_seek_free() {
        let test_dir = PathBuf::from("target/tmp/cbz_seek_free_test");
        let _ = fs::remove_dir_all(&test_dir);
        fs::create_dir_all(&test_dir).unwrap();

        let zip_path = test_dir.join("test_comic.cbz");
        let file = fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);

        let options = zip::write::SimpleFileOptions::default();

        // 1. Directory entry (must be skipped)
        zip.add_directory("covers/", options).unwrap();

        // 2. Non-image files (must be skipped)
        zip.start_file("ComicInfo.xml", options).unwrap();
        zip.write_all(b"<ComicInfo></ComicInfo>").unwrap();

        zip.start_file("notes.txt", options).unwrap();
        zip.write_all(b"Some comic notes").unwrap();

        // 3. Mixed-case image files in unsorted order
        zip.start_file("page10.jpg", options).unwrap();
        zip.write_all(b"fake jpeg").unwrap();

        zip.start_file("Page02.PNG", options).unwrap();
        zip.write_all(b"fake png").unwrap();

        zip.start_file("page01.webp", options).unwrap();
        zip.write_all(b"fake webp").unwrap();

        zip.finish().unwrap();

        let pages = list_cbz_pages(&zip_path).expect("Failed to list cbz pages");

        assert_eq!(pages, vec!["page01.webp", "Page02.PNG", "page10.jpg"]);

        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_cancellation_token_behavior() {
        let flag = AtomicBool::new(false);
        let token = CancellationToken::from_flag(&flag);
        assert!(!token.is_cancelled());
        token.cancel();
        assert!(token.is_cancelled());

        let active_seq = AtomicUsize::new(10);
        let seq_token = CancellationToken::with_sequence(10, &active_seq);
        assert!(!seq_token.is_cancelled());

        // Advance active sequence past 10
        active_seq.store(11, Ordering::Release);
        assert!(seq_token.is_cancelled());
    }

    #[test]
    fn test_decode_image_bytes_cancellation_checkpoint() {
        let flag = AtomicBool::new(true);
        let token = CancellationToken::from_flag(&flag);

        let dummy_bytes = b"not a real image";
        let source = ImageSource::Local(PathBuf::from("test.jpg"));

        let result = decode_image_bytes_cancellable(dummy_bytes, &source, &token);
        match result {
            Err(e) => assert_eq!(e, "Operation cancelled"),
            Ok(_) => panic!("Expected cancellation error"),
        }
    }

    #[test]
    fn test_parse_tolerant_exif_safety() {
        assert!(parse_tolerant_exif(&[]).is_none());
        assert!(parse_tolerant_exif(b"random-non-exif-data").is_none());

        // Partial TIFF header without complete IFD
        let tiff_partial = [0x49, 0x49, 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00];
        let _ = parse_tolerant_exif(&tiff_partial);

        // 1. Synthetic TIFF buffer with a valid IFD0 entry (Orientation = 6)
        // followed by a corrupted entry with an out-of-bounds offset that strict Reader fails on
        let mut malformed_tiff = Vec::new();
        // TIFF header: "II", magic 42, IFD0 offset at 8
        malformed_tiff.extend_from_slice(&[0x49, 0x49, 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00]);
        // IFD0: 2 entries
        malformed_tiff.extend_from_slice(&[0x02, 0x00]);
        // Entry 1: Tag 0x0112 (Orientation), Type 3 (SHORT), Count 1, Value 6
        malformed_tiff.extend_from_slice(&[
            0x12, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00,
        ]);
        // Entry 2: Tag 0x0100 (ImageWidth), Type 4 (LONG), Count 100, Offset 0x00010000 (65536, out of bounds)
        malformed_tiff.extend_from_slice(&[
            0x00, 0x01, 0x04, 0x00, 0x64, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00,
        ]);
        // Next IFD: 0
        malformed_tiff.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);

        // Strict reader fails due to unexpected EOF reading the out-of-bounds entry
        let mut strict_cursor = std::io::Cursor::new(&malformed_tiff);
        assert!(
            exif::Reader::new()
                .read_from_container(&mut strict_cursor)
                .is_err()
        );

        // parse_tolerant_exif distills the partial result and extracts the valid Orientation tag
        let exif = parse_tolerant_exif(&malformed_tiff)
            .expect("tolerant exif should distill partial result");
        let orientation = exif
            .get_field(exif::Tag::Orientation, exif::In::PRIMARY)
            .expect("orientation tag");
        assert_eq!(orientation.value.get_uint(0), Some(6));

        // 2. Synthetic buffer > 256KB (300KB) with JPEG preview at offset 280KB (> 256KB probe limit)
        let mut large_raw = vec![0u8; 300 * 1024];
        // TIFF header at 0
        large_raw[0..8].copy_from_slice(&[0x49, 0x49, 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00]);
        // IFD0: 2 entries at offset 8
        large_raw[8..10].copy_from_slice(&[0x02, 0x00]);
        let jpeg_offset = 280 * 1024u32;
        let jpeg_len = 64u32;
        // Entry 1: Tag 0x0201 (JPEGInterchangeFormat), Type 4 (LONG), Count 1, Value = 280KB
        large_raw[10..22].copy_from_slice(&[
            0x01,
            0x02,
            0x04,
            0x00,
            0x01,
            0x00,
            0x00,
            0x00,
            (jpeg_offset & 0xFF) as u8,
            ((jpeg_offset >> 8) & 0xFF) as u8,
            ((jpeg_offset >> 16) & 0xFF) as u8,
            ((jpeg_offset >> 24) & 0xFF) as u8,
        ]);
        // Entry 2: Tag 0x0202 (JPEGInterchangeFormatLength), Type 4 (LONG), Count 1, Value = 64
        large_raw[22..34].copy_from_slice(&[
            0x02,
            0x02,
            0x04,
            0x00,
            0x01,
            0x00,
            0x00,
            0x00,
            (jpeg_len & 0xFF) as u8,
            ((jpeg_len >> 8) & 0xFF) as u8,
            ((jpeg_len >> 16) & 0xFF) as u8,
            ((jpeg_len >> 24) & 0xFF) as u8,
        ]);
        // Next IFD: 0
        large_raw[34..38].copy_from_slice(&[0x00, 0x00, 0x00, 0x00]);

        // Place synthetic JPEG payload at 280KB
        let off = jpeg_offset as usize;
        large_raw[off..off + 4].copy_from_slice(&[0xFF, 0xD8, 0xFF, 0xE0]);
        for i in 4..jpeg_len as usize {
            large_raw[off + i] = 0xAA;
        }

        let preview = extract_jpeg_preview(&large_raw)
            .expect("extract_jpeg_preview must extract preview past 256KB");
        assert_eq!(preview.len(), 64);
        assert_eq!(&preview[0..4], &[0xFF, 0xD8, 0xFF, 0xE0]);

        // Test with ImageSource reading via read_raw_preview_bytes (2MB probe limit)
        let test_dir = PathBuf::from("target/tmp/exif_probe_test");
        let _ = fs::remove_dir_all(&test_dir);
        fs::create_dir_all(&test_dir).unwrap();
        let raw_path = test_dir.join("sample.dng");
        fs::write(&raw_path, &large_raw).unwrap();

        let source = ImageSource::Local(raw_path);
        let extracted = read_raw_preview_bytes(&source)
            .expect("read_raw_preview_bytes must succeed with 2MB probe limit");
        assert_eq!(extracted.len(), 64);
        assert_eq!(&extracted[0..4], &[0xFF, 0xD8, 0xFF, 0xE0]);

        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_convenience_decode_helpers() {
        let dummy_bytes = b"not an image";
        let source = ImageSource::Local(PathBuf::from("nonexistent.jpg"));
        assert!(decode_image_bytes(dummy_bytes, &source).is_err());
        assert!(decode_image_source(source).is_err());
    }

    #[test]
    fn test_unpack_rgb24_to_rgba32_simd_and_tail_equivalence() {
        // Test varying pixel counts:
        // - Empty: 0
        // - Tail-only: 1, 2, 3
        // - Exact multiple of 4: 4, 16
        // - Non-multiple of 4: 5, 7, 19, 33
        let pixel_counts = [0, 1, 2, 3, 4, 5, 7, 16, 19, 33];

        for &count in &pixel_counts {
            let mut rgb_bytes = Vec::with_capacity(count * 3);
            for i in 0..count {
                rgb_bytes.push(((i * 7 + 13) % 256) as u8);
                rgb_bytes.push(((i * 11 + 47) % 256) as u8);
                rgb_bytes.push(((i * 13 + 89) % 256) as u8);
            }

            let rgba = unpack_rgb24_to_rgba32(&rgb_bytes);
            assert_eq!(rgba.len(), count * 4);

            for i in 0..count {
                assert_eq!(rgba[i * 4], rgb_bytes[i * 3]);
                assert_eq!(rgba[i * 4 + 1], rgb_bytes[i * 3 + 1]);
                assert_eq!(rgba[i * 4 + 2], rgb_bytes[i * 3 + 2]);
                assert_eq!(rgba[i * 4 + 3], 255);
            }

            if count > 0 {
                let rgb_img =
                    image::RgbImage::from_raw(count as u32, 1, rgb_bytes.clone()).unwrap();
                let dynamic = DynamicImage::ImageRgb8(rgb_img);
                let std_rgba = dynamic.clone().into_rgba8();
                assert_eq!(rgba, std_rgba.into_raw());

                let fast_rgba = into_rgba8_fast(dynamic.clone());
                assert_eq!(fast_rgba.as_raw(), &rgba);

                let cow_rgba = to_rgba8_fast(&dynamic);
                assert_eq!(cow_rgba.as_raw(), &rgba);
            }
        }

        // Test 2D dimensions equivalence (e.g., 7x5, 16x3)
        let dimensions = [(7, 5), (16, 3), (1, 1), (4, 4), (13, 9)];
        for (w, h) in dimensions {
            let count = (w * h) as usize;
            let mut rgb_bytes = Vec::with_capacity(count * 3);
            for i in 0..count {
                rgb_bytes.push(((i * 3 + 5) % 256) as u8);
                rgb_bytes.push(((i * 5 + 17) % 256) as u8);
                rgb_bytes.push(((i * 7 + 31) % 256) as u8);
            }

            let fast_unpacked = unpack_rgb24_to_rgba32(&rgb_bytes);
            let rgb_img = image::RgbImage::from_raw(w, h, rgb_bytes).unwrap();
            let dynamic = DynamicImage::ImageRgb8(rgb_img);
            let std_rgba = dynamic.clone().into_rgba8();

            assert_eq!(fast_unpacked, std_rgba.into_raw());

            let fast_owned = into_rgba8_fast(dynamic.clone());
            assert_eq!(fast_owned.as_raw(), &fast_unpacked);

            let cow_borrowed_or_owned = to_rgba8_fast(&dynamic);
            assert_eq!(cow_borrowed_or_owned.as_raw(), &fast_unpacked);

            // Also test to_rgba8_fast on an existing DynamicImage::ImageRgba8 borrows directly
            let dynamic_rgba = DynamicImage::ImageRgba8(fast_owned);
            match to_rgba8_fast(&dynamic_rgba) {
                std::borrow::Cow::Borrowed(_) => {}
                std::borrow::Cow::Owned(_) => panic!("Expected Cow::Borrowed for ImageRgba8"),
            }
        }
    }

    #[test]
    fn test_raw_header_reuse_deduplicated_helpers_and_fail_fast() {
        // 1. Verify is_tiff_header
        assert!(is_tiff_header(&[0x49, 0x49, 0x2A, 0x00]));
        assert!(is_tiff_header(&[0x4D, 0x4D, 0x00, 0x2A]));
        assert!(!is_tiff_header(&[0x49, 0x49, 0x00, 0x2A]));
        assert!(!is_tiff_header(&[0xFF, 0xD8, 0xFF, 0xE0]));
        assert!(!is_tiff_header(b"PNG"));
        assert!(!is_tiff_header(&[]));

        // 2. Verify orientation_from_exif_value
        assert_eq!(
            orientation_from_exif_value(1),
            image::metadata::Orientation::NoTransforms
        );
        assert_eq!(
            orientation_from_exif_value(2),
            image::metadata::Orientation::FlipHorizontal
        );
        assert_eq!(
            orientation_from_exif_value(3),
            image::metadata::Orientation::Rotate180
        );
        assert_eq!(
            orientation_from_exif_value(4),
            image::metadata::Orientation::FlipVertical
        );
        assert_eq!(
            orientation_from_exif_value(5),
            image::metadata::Orientation::Rotate90FlipH
        );
        assert_eq!(
            orientation_from_exif_value(6),
            image::metadata::Orientation::Rotate90
        );
        assert_eq!(
            orientation_from_exif_value(7),
            image::metadata::Orientation::Rotate270FlipH
        );
        assert_eq!(
            orientation_from_exif_value(8),
            image::metadata::Orientation::Rotate270
        );
        assert_eq!(
            orientation_from_exif_value(0),
            image::metadata::Orientation::NoTransforms
        );
        assert_eq!(
            orientation_from_exif_value(99),
            image::metadata::Orientation::NoTransforms
        );

        // 3. Verify fail-fast behavior when preview is missing from .arw / .raf files
        let test_dir = PathBuf::from("target/tmp/raw_fail_fast_test");
        let _ = fs::remove_dir_all(&test_dir);
        fs::create_dir_all(&test_dir).unwrap();

        let arw_path = test_dir.join("corrupt.arw");
        fs::write(&arw_path, b"corrupt raw sensor bytes without preview").unwrap();
        let arw_source = ImageSource::Local(arw_path);
        let arw_res = read_source_bytes(&arw_source);
        assert!(arw_res.is_err());
        assert!(
            arw_res
                .unwrap_err()
                .contains("Failed to extract embedded JPEG preview from RAW file")
        );

        let raf_path = test_dir.join("corrupt.raf");
        fs::write(&raf_path, b"corrupt fuji raw bytes without preview").unwrap();
        let raf_source = ImageSource::Local(raf_path);
        let raf_res = read_source_bytes(&raf_source);
        assert!(raf_res.is_err());
        assert!(
            raf_res
                .unwrap_err()
                .contains("Failed to extract embedded JPEG preview from RAW file")
        );

        // Encode a non-square 10x20 JPEG preview
        let mut jpeg_bytes = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::new(10, 20))
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg_bytes),
                image::ImageFormat::Jpeg,
            )
            .unwrap();

        // Test valid .raf preview extraction via read_source_bytes_with_header
        let raf_valid_path = test_dir.join("valid.raf");
        let mut raf_data = vec![0u8; 128 + jpeg_bytes.len()];
        raf_data[0..8].copy_from_slice(b"FUJIFILM");
        raf_data[84..88].copy_from_slice(&128u32.to_be_bytes());
        raf_data[88..92].copy_from_slice(&(jpeg_bytes.len() as u32).to_be_bytes());
        raf_data[128..].copy_from_slice(&jpeg_bytes);
        fs::write(&raf_valid_path, &raf_data).unwrap();

        let raf_valid_source = ImageSource::Local(raf_valid_path);
        let raf_header_prefix = &raf_data[..128];
        let extracted = read_source_bytes_with_header(&raf_valid_source, Some(raf_header_prefix))
            .expect("read_source_bytes_with_header must extract valid preview");
        assert_eq!(extracted, jpeg_bytes);

        let _ = fs::remove_dir_all(&test_dir);

        // 4. Verify decode_image_bytes_with_header_cancellable with pre-supplied header_bytes
        // proving raw_container_orientation and dimension swap without filesystem access
        let mut header = vec![0u8; 128];
        // TIFF header: II*\0, offset to IFD0 = 8
        header[0..8].copy_from_slice(&[0x49, 0x49, 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00]);
        // IFD0 count: 3 tags
        header[8..10].copy_from_slice(&[0x03, 0x00]);
        // Tag 1: ImageWidth (0x0100), Type 4 (LONG), Count 1, Value = 6000
        header[10..22].copy_from_slice(&[
            0x00, 0x01, 0x04, 0x00, 0x01, 0x00, 0x00, 0x00, 0x70, 0x17, 0x00, 0x00,
        ]);
        // Tag 2: ImageLength (0x0101), Type 4 (LONG), Count 1, Value = 4000
        header[22..34].copy_from_slice(&[
            0x01, 0x01, 0x04, 0x00, 0x01, 0x00, 0x00, 0x00, 0xA0, 0x0F, 0x00, 0x00,
        ]);
        // Tag 3: Orientation (0x0112), Type 3 (SHORT), Count 1, Value = 6 (Rotate90)
        header[34..46].copy_from_slice(&[
            0x12, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00,
        ]);
        // Next IFD: 0
        header[46..50].copy_from_slice(&[0x00, 0x00, 0x00, 0x00]);

        let source = ImageSource::Local(PathBuf::from("synthetic_camera.arw"));
        let decoded = decode_image_bytes_with_header_cancellable(
            &jpeg_bytes,
            &source,
            Some(&header),
            &CancellationToken::none(),
        )
        .expect("decode_image_bytes_with_header_cancellable must succeed");

        // Orientation Rotate90 swaps raw dimensions from 6000x4000 to 4000x6000
        assert_eq!(decoded.raw_dimensions, Some((4000, 6000)));
        // Non-square 10x20 preview was rotated 90° clockwise into 20x10 using in-memory header orientation
        assert_eq!(decoded.width, 20);
        assert_eq!(decoded.height, 10);
    }

    #[test]
    fn test_raw_strip_offsets_and_sof3_lossless_rejection() {
        // 1. Test is_decodable_jpeg
        assert!(!is_decodable_jpeg(&[]));
        assert!(!is_decodable_jpeg(b"PNG\r\n\x1a\n"));
        assert!(!is_decodable_jpeg(&[0xFF, 0xD8, 0x00, 0x00]));

        // Baseline SOF0 (0xFF, 0xC0)
        let mut baseline_jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        baseline_jpeg.extend_from_slice(&[0; 14]);
        baseline_jpeg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08, 0x00, 0x0A, 0x00, 0x0A]);
        assert!(is_decodable_jpeg(&baseline_jpeg));

        // Progressive SOF2 (0xFF, 0xC2)
        let mut prog_jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        prog_jpeg.extend_from_slice(&[0; 14]);
        prog_jpeg.extend_from_slice(&[0xFF, 0xC2, 0x00, 0x11, 0x08, 0x00, 0x0A, 0x00, 0x0A]);
        assert!(is_decodable_jpeg(&prog_jpeg));

        // Lossless SOF3 (0xFF, 0xC3) -> must be rejected!
        let mut lossless_jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        lossless_jpeg.extend_from_slice(&[0; 14]);
        lossless_jpeg.extend_from_slice(&[0xFF, 0xC3, 0x00, 0x11, 0x08, 0x00, 0x0A, 0x00, 0x0A]);
        assert!(!is_decodable_jpeg(&lossless_jpeg));

        // 2. Synthetic TIFF container with two candidates:
        // Candidate 1: larger (500 bytes) SOF3 lossless stream via JPEGInterchangeFormat
        // Candidate 2: smaller (200 bytes) SOF0 baseline stream via StripOffsets / StripByteCounts
        let mut container = vec![0u8; 1024];
        // TIFF header: II*\0, offset to IFD0 = 8
        container[0..8].copy_from_slice(&[0x49, 0x49, 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00]);
        // IFD0 count: 4 tags
        container[8..10].copy_from_slice(&[0x04, 0x00]);
        // Tag 1: JPEGInterchangeFormat (0x0201), Type 4 (LONG), Count 1, Value = 100
        container[10..22].copy_from_slice(&[
            0x01, 0x02, 0x04, 0x00, 0x01, 0x00, 0x00, 0x00, 100, 0x00, 0x00, 0x00,
        ]);
        // Tag 2: JPEGInterchangeFormatLength (0x0202), Type 4 (LONG), Count 1, Value = 500
        container[22..34].copy_from_slice(&[
            0x02, 0x02, 0x04, 0x00, 0x01, 0x00, 0x00, 0x00, 0xF4, 0x01, 0x00, 0x00,
        ]);
        // Tag 3: StripOffsets (0x0111), Type 4 (LONG), Count 1, Value = 700
        container[34..46].copy_from_slice(&[
            0x11, 0x01, 0x04, 0x00, 0x01, 0x00, 0x00, 0x00, 0xBC, 0x02, 0x00, 0x00,
        ]);
        // Tag 4: StripByteCounts (0x0117), Type 4 (LONG), Count 1, Value = 200
        container[46..58].copy_from_slice(&[
            0x17, 0x01, 0x04, 0x00, 0x01, 0x00, 0x00, 0x00, 200, 0x00, 0x00, 0x00,
        ]);
        // Next IFD: 0
        container[58..62].copy_from_slice(&[0x00, 0x00, 0x00, 0x00]);

        // Place SOF3 lossless payload at offset 100..600 (500 bytes)
        container[100..100 + lossless_jpeg.len()].copy_from_slice(&lossless_jpeg);

        // Place SOF0 baseline payload at offset 700..900 (200 bytes)
        container[700..700 + baseline_jpeg.len()].copy_from_slice(&baseline_jpeg);

        // extract_jpeg_preview sorts descending by length: tries 500-byte SOF3 first, rejects it,
        // and returns the 200-byte SOF0 baseline JPEG preview!
        let extracted_preview = extract_jpeg_preview(&container)
            .expect("extract_jpeg_preview must skip SOF3 and extract SOF0 preview");
        assert_eq!(extracted_preview.len(), 200);
        assert_eq!(
            &extracted_preview[..baseline_jpeg.len()],
            &baseline_jpeg[..]
        );

        // Also test read_raw_preview_bytes_with_header on synthetic ImageSource
        let source = ImageSource::Local(PathBuf::from("mock.dng"));
        let extracted_source = read_raw_preview_bytes_with_header(&source, &container)
            .expect("read_raw_preview_bytes_with_header must extract in-memory SOF0 preview");
        assert_eq!(extracted_source.len(), 200);
        assert_eq!(&extracted_source[..baseline_jpeg.len()], &baseline_jpeg[..]);
    }

    #[test]
    fn test_jpeg_and_rgb_decode_preserves_rgb8_format() {
        // 1. Synthetic RGB image
        let rgb_raw = image::RgbImage::from_fn(16, 16, |x, y| {
            image::Rgb([(x * 16) as u8, (y * 16) as u8, 128])
        });

        // 2. Encode to JPEG
        let mut jpeg_bytes = Vec::new();
        DynamicImage::ImageRgb8(rgb_raw.clone())
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg_bytes),
                image::ImageFormat::Jpeg,
            )
            .unwrap();

        // 3. Encode to RGB PNG
        let mut rgb_png_bytes = Vec::new();
        DynamicImage::ImageRgb8(rgb_raw)
            .write_to(
                &mut std::io::Cursor::new(&mut rgb_png_bytes),
                image::ImageFormat::Png,
            )
            .unwrap();

        // 4. Encode to RGBA PNG
        let rgba_raw = image::RgbaImage::from_fn(16, 16, |x, y| {
            image::Rgba([(x * 16) as u8, (y * 16) as u8, 128, 200])
        });
        let mut rgba_png_bytes = Vec::new();
        DynamicImage::ImageRgba8(rgba_raw)
            .write_to(
                &mut std::io::Cursor::new(&mut rgba_png_bytes),
                image::ImageFormat::Png,
            )
            .unwrap();

        // 5. Verify decode_image_bytes outputs DynamicImage::ImageRgb8 for JPEG and RGB PNG
        let src_jpg = ImageSource::Local(PathBuf::from("test.jpg"));
        let decoded_jpg = decode_image_bytes(&jpeg_bytes, &src_jpg).expect("JPEG decode failed");
        assert!(
            matches!(decoded_jpg.image, DynamicImage::ImageRgb8(_)),
            "Expected JPEG to decode directly as DynamicImage::ImageRgb8"
        );

        let src_rgb_png = ImageSource::Local(PathBuf::from("test_rgb.png"));
        let decoded_rgb_png =
            decode_image_bytes(&rgb_png_bytes, &src_rgb_png).expect("RGB PNG decode failed");
        assert!(
            matches!(decoded_rgb_png.image, DynamicImage::ImageRgb8(_)),
            "Expected RGB PNG to decode directly as DynamicImage::ImageRgb8"
        );

        // 6. Verify decode_image_bytes preserves DynamicImage::ImageRgba8 for RGBA PNG
        let src_rgba_png = ImageSource::Local(PathBuf::from("test_rgba.png"));
        let decoded_rgba_png =
            decode_image_bytes(&rgba_png_bytes, &src_rgba_png).expect("RGBA PNG decode failed");
        assert!(
            matches!(decoded_rgba_png.image, DynamicImage::ImageRgba8(_)),
            "Expected RGBA PNG to decode as DynamicImage::ImageRgba8"
        );

        // 7. Verify embedded JPEG thumbnail decoding uses zune_jpeg and preserves ImageRgb8
        let decoded_thumb =
            decode_jpeg_thumbnail_bytes(&jpeg_bytes).expect("thumbnail decode failed");
        assert!(
            matches!(decoded_thumb, DynamicImage::ImageRgb8(_)),
            "Expected embedded JPEG thumbnail to decode directly as DynamicImage::ImageRgb8"
        );
    }
}
