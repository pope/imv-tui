//! High-level image decoding logic, file type guessing, and source scanning.

use std::fs;
use std::path::{Path, PathBuf};

use crate::imaging::DecodedImage;
use crate::imaging::ImageSource;
use crate::imaging::raw::*;
use image::{DynamicImage, ImageDecoder};

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

/// Checks if a file path points to a supported image.
pub fn is_image_file(path: &Path, check_magic: bool) -> bool {
    if !path.is_file() {
        return false;
    }
    if check_magic {
        matches!(guess_file_type(path), Some(GuessType::Image))
    } else {
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
                    || ext.eq_ignore_ascii_case("dng")
                    || ext.eq_ignore_ascii_case("cr2")
                    || ext.eq_ignore_ascii_case("nef")
                    || ext.eq_ignore_ascii_case("arw")
                    || ext.eq_ignore_ascii_case("orf")
                    || ext.eq_ignore_ascii_case("rw2")
                    || ext.eq_ignore_ascii_case("pef")
                    || ext.eq_ignore_ascii_case("raf")
            })
            .unwrap_or(false)
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
    let reader = std::io::BufReader::new(file);
    let mut archive = zip::ZipArchive::new(reader)
        .map_err(|e| format!("Failed to read zip archive {}: {}", zip_path.display(), e))?;

    let mut pages = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i)
            && entry.is_file()
        {
            let name = entry.name();
            if let Some(ext) = Path::new(name).extension().and_then(|e| e.to_str())
                && (ext.eq_ignore_ascii_case("png")
                    || ext.eq_ignore_ascii_case("jpg")
                    || ext.eq_ignore_ascii_case("jpeg")
                    || ext.eq_ignore_ascii_case("gif")
                    || ext.eq_ignore_ascii_case("webp")
                    || ext.eq_ignore_ascii_case("bmp")
                    || ext.eq_ignore_ascii_case("tiff")
                    || ext.eq_ignore_ascii_case("ico"))
            {
                pages.push(name.to_string());
            }
        }
    }

    pages.sort_by_cached_key(|a| a.to_lowercase());
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

/// Decodes an image from already loaded memory bytes.
pub fn decode_image_bytes(bytes: &[u8], source: &ImageSource) -> Result<DecodedImage, String> {
    let file_size = match source {
        ImageSource::Local(path) => std::fs::metadata(path)
            .map(|m| m.len())
            .unwrap_or(bytes.len() as u64),
        _ => bytes.len() as u64,
    };
    let format = image::guess_format(bytes).ok();
    let display_name = source.display_name();

    // Intercept RAW/TIFF files to extract and decode their embedded JPEG preview/thumbnail
    let is_raw_extension = match source {
        ImageSource::Local(path) => path.extension(),
        ImageSource::Cbz { zip_path, .. } => zip_path.extension(),
    }
    .and_then(|ext| ext.to_str())
    .map(|ext| {
        ext.eq_ignore_ascii_case("dng")
            || ext.eq_ignore_ascii_case("cr2")
            || ext.eq_ignore_ascii_case("nef")
            || ext.eq_ignore_ascii_case("arw")
            || ext.eq_ignore_ascii_case("orf")
            || ext.eq_ignore_ascii_case("rw2")
            || ext.eq_ignore_ascii_case("pef")
            || ext.eq_ignore_ascii_case("raf")
    })
    .unwrap_or(false);

    let mut raw_width = None;
    let mut raw_height = None;
    if is_raw_extension {
        // Read the first 256 KB of the raw container file on disk to inspect its container format
        if let Ok(partial_bytes) = read_source_bytes_limited(source, 256 * 1024) {
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
                    raw_width = Some(h);
                    raw_height = Some(w);
                } else {
                    raw_width = Some(w);
                    raw_height = Some(h);
                }
            } else {
                // If not Fuji RAF, try standard TIFF EXIF container check
                let mut cursor = std::io::Cursor::new(&partial_bytes);
                if let Ok(exif) = exif::Reader::new().read_from_container(&mut cursor)
                    && let Some((w, h)) = get_dimensions_from_exif(&exif)
                {
                    let orientation = exif
                        .get_field(exif::Tag::Orientation, exif::In::PRIMARY)
                        .and_then(|f| match f.value {
                            exif::Value::Short(ref v) => v.first().copied(),
                            _ => None,
                        })
                        .map(|val| match val {
                            2 => image::metadata::Orientation::FlipHorizontal,
                            3 => image::metadata::Orientation::Rotate180,
                            4 => image::metadata::Orientation::FlipVertical,
                            5 => image::metadata::Orientation::Rotate90FlipH,
                            6 => image::metadata::Orientation::Rotate90,
                            7 => image::metadata::Orientation::Rotate270FlipH,
                            8 => image::metadata::Orientation::Rotate270,
                            _ => image::metadata::Orientation::NoTransforms,
                        })
                        .unwrap_or(image::metadata::Orientation::NoTransforms);
                    let swaps = matches!(
                        orientation,
                        image::metadata::Orientation::Rotate90
                            | image::metadata::Orientation::Rotate270
                            | image::metadata::Orientation::Rotate90FlipH
                            | image::metadata::Orientation::Rotate270FlipH
                    );
                    if swaps {
                        raw_width = Some(h);
                        raw_height = Some(w);
                    } else {
                        raw_width = Some(w);
                        raw_height = Some(h);
                    }
                }
            }
        }
    }

    if is_raw_extension && (raw_width.is_none() || raw_height.is_none()) {
        let mut cursor = std::io::Cursor::new(bytes);
        if let Ok(exif) = exif::Reader::new().read_from_container(&mut cursor)
            && let Some((w, h)) = get_dimensions_from_exif(&exif)
        {
            let orientation = exif
                .get_field(exif::Tag::Orientation, exif::In::PRIMARY)
                .and_then(|f| match f.value {
                    exif::Value::Short(ref v) => v.first().copied(),
                    _ => None,
                })
                .map(|val| match val {
                    2 => image::metadata::Orientation::FlipHorizontal,
                    3 => image::metadata::Orientation::Rotate180,
                    4 => image::metadata::Orientation::FlipVertical,
                    5 => image::metadata::Orientation::Rotate90FlipH,
                    6 => image::metadata::Orientation::Rotate90,
                    7 => image::metadata::Orientation::Rotate270FlipH,
                    8 => image::metadata::Orientation::Rotate270,
                    _ => image::metadata::Orientation::NoTransforms,
                })
                .unwrap_or(image::metadata::Orientation::NoTransforms);
            let swaps = matches!(
                orientation,
                image::metadata::Orientation::Rotate90
                    | image::metadata::Orientation::Rotate270
                    | image::metadata::Orientation::Rotate90FlipH
                    | image::metadata::Orientation::Rotate270FlipH
            );
            if swaps {
                raw_width = Some(h);
                raw_height = Some(w);
            } else {
                raw_width = Some(w);
                raw_height = Some(h);
            }
        }
    }

    if format != Some(image::ImageFormat::Jpeg)
        && (format == Some(image::ImageFormat::Tiff) || is_raw_extension)
        && let Some(jpeg_bytes) = extract_jpeg_preview(bytes)
    {
        // Recurse using the extracted JPEG bytes
        if let Ok(mut decoded) = decode_image_bytes(&jpeg_bytes, source) {
            decoded.format = Some(image::ImageFormat::Jpeg);
            decoded.disk_size = file_size;
            if raw_width.is_some() {
                decoded.raw_width = raw_width;
            }
            if raw_height.is_some() {
                decoded.raw_height = raw_height;
            }
            return Ok(decoded);
        }
    }

    if let Some(image::ImageFormat::Jpeg) = format {
        let options = zune_jpeg::zune_core::options::DecoderOptions::default()
            .jpeg_set_out_colorspace(zune_jpeg::zune_core::colorspace::ColorSpace::RGBA);
        let mut decoder = zune_jpeg::JpegDecoder::new_with_options(bytes, options);
        if let Ok(pixels) = decoder.decode()
            && let Some(info) = decoder.info()
            && let Some(rgba_img) =
                image::RgbaImage::from_raw(info.width as u32, info.height as u32, pixels)
        {
            let mut orientation = get_exif_orientation(bytes);
            if orientation == image::metadata::Orientation::NoTransforms && is_raw_extension {
                orientation = read_source_bytes_limited(source, 256 * 1024)
                    .ok()
                    .map(|pb| get_exif_orientation(&pb))
                    .unwrap_or(image::metadata::Orientation::NoTransforms);
            }

            let mut img = image::DynamicImage::ImageRgba8(rgba_img);
            img.apply_orientation(orientation);
            let w = img.width();
            let h = img.height();
            let final_raw_width = raw_width.unwrap_or(w);
            let final_raw_height = raw_height.unwrap_or(h);
            return Ok(DecodedImage {
                image: img,
                width: w,
                height: h,
                format: Some(image::ImageFormat::Jpeg),
                disk_size: file_size,
                raw_width: Some(final_raw_width),
                raw_height: Some(final_raw_height),
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
    let mut img = image::DynamicImage::from_decoder(decoder)
        .map_err(|e| format!("Failed to decode image:\n{}\n\nError: {}", display_name, e))?;

    img.apply_orientation(orientation);
    let rgba_img = img.into_rgba8();
    let w = rgba_img.width();
    let h = rgba_img.height();
    let final_raw_width = raw_width.unwrap_or(w);
    let final_raw_height = raw_height.unwrap_or(h);
    Ok(DecodedImage {
        image: image::DynamicImage::ImageRgba8(rgba_img),
        width: w,
        height: h,
        format,
        disk_size: file_size,
        raw_width: Some(final_raw_width),
        raw_height: Some(final_raw_height),
    })
}

/// Decodes an image from local paths or comic book archives.
/// Employs zune-jpeg for extremely fast decoding of JPEGs.
pub fn decode_image_source(source: ImageSource) -> Result<DecodedImage, String> {
    let bytes = read_source_bytes(&source)?;
    decode_image_bytes(&bytes, &source)
}

/// Reads the image dimensions extremely quickly from header headers, and decodes/scales
/// its embedded EXIF thumbnail to serve as a fast low-res placeholder for large cold loads.
pub fn decode_thumbnail_and_dimensions(bytes: &[u8]) -> Option<(DynamicImage, u32, u32)> {
    // Check if this is a TIFF/RAW container
    let is_tiff = bytes.len() >= 4
        && ((bytes[0] == 0x49 && bytes[1] == 0x49 && bytes[2] == 0x2A && bytes[3] == 0x00)
            || (bytes[0] == 0x4D && bytes[1] == 0x4D && bytes[2] == 0x00 && bytes[3] == 0x2A));

    if is_tiff {
        let mut cursor = std::io::Cursor::new(bytes);
        let exif = exif::Reader::new().read_from_container(&mut cursor).ok()?;

        let thumb_bytes = extract_jpeg_thumbnail(bytes)?;
        let mut thumb_img = image::load_from_memory(&thumb_bytes).ok()?;

        let orientation = exif
            .get_field(exif::Tag::Orientation, exif::In::PRIMARY)
            .and_then(|f| match f.value {
                exif::Value::Short(ref v) => v.first().copied(),
                _ => None,
            })
            .map(|val| match val {
                2 => image::metadata::Orientation::FlipHorizontal,
                3 => image::metadata::Orientation::Rotate180,
                4 => image::metadata::Orientation::FlipVertical,
                5 => image::metadata::Orientation::Rotate90FlipH,
                6 => image::metadata::Orientation::Rotate90,
                7 => image::metadata::Orientation::Rotate270FlipH,
                8 => image::metadata::Orientation::Rotate270,
                _ => image::metadata::Orientation::NoTransforms,
            })
            .unwrap_or(image::metadata::Orientation::NoTransforms);

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

        return Some((cropped_thumb, raw_w, raw_h));
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
    let mut thumb_img = image::load_from_memory(&thumb_bytes).ok()?;

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

    Some((cropped_thumb, oriented_raw_w, oriented_raw_h))
}
