pub mod types;
pub use types::*;

use fast_image_resize as fir;
use image::{DynamicImage, GenericImage, ImageDecoder};
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The source input format for an image.
#[derive(Clone, Debug)]
pub enum ImageSource {
    /// A local filesystem image path.
    Local(PathBuf),
    /// A page inside a comic book zip archive (CBZ).
    Cbz {
        /// Path to the zip archive.
        zip_path: PathBuf,
        /// Name of the image entry inside the zip file.
        file_in_zip: String,
    },
}

impl ImageSource {
    /// Returns a unique absolute identifier string for the image source (local path or cbz path + page).
    pub fn identifier(&self) -> String {
        match self {
            Self::Local(path) => {
                let abs = if path.is_absolute() {
                    path.clone()
                } else if let Ok(curr) = std::env::current_dir() {
                    curr.join(path)
                } else {
                    path.clone()
                };
                abs.to_string_lossy().into_owned()
            }
            Self::Cbz {
                zip_path,
                file_in_zip,
            } => {
                let abs_zip = if zip_path.is_absolute() {
                    zip_path.clone()
                } else if let Ok(curr) = std::env::current_dir() {
                    curr.join(zip_path)
                } else {
                    zip_path.clone()
                };
                format!("{}::{}", abs_zip.to_string_lossy(), file_in_zip)
            }
        }
    }

    /// Generates a display name for listing the image source in the status bar/palettes.
    pub fn display_name(&self) -> String {
        match self {
            Self::Local(path) => path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("Unknown")
                .to_string(),
            Self::Cbz {
                zip_path,
                file_in_zip,
            } => {
                let zip_name = zip_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("Unknown");
                format!("{}: {}", zip_name, file_in_zip)
            }
        }
    }
}

/// A request payload sent to the resizing worker threads.
pub struct ResizeRequest {
    /// Shared reference to the decoded dynamic image.
    pub img: Arc<DynamicImage>,
    /// Expected full-resolution dimensions of the image.
    /// If the actual image buffer dimensions differ, it indicates a thumbnail placeholder.
    pub original_size: (u32, u32),
    /// Calculated scaling factor.
    pub scale: f64,
    /// Crop box geometry.
    pub crop: CropBox,
    /// Clamped image intersection region.
    pub intersection: ImageIntersection,
    /// Resized target width.
    pub target_w: u32,
    /// Resized target height.
    pub target_h: u32,
    /// Desired scaling filter.
    pub filter_type: FilterType,
    /// Protocol generator picker.
    pub picker: Picker,
    /// Brightness correction value.
    pub brightness: Brightness,
    /// Contrast correction percentage.
    pub contrast: Contrast,
    /// Output terminal grid cell dimensions.
    pub rendered_size_cells: (u16, u16),
    /// Sequence identifier to filter out stale requests.
    pub sequence: u64,
}

/// A request sent to the persistent loader thread.
pub struct LoaderRequest {
    /// File index inside the App source list.
    pub idx: usize,
    /// The ImageSource identifier.
    pub source: ImageSource,
    /// Whether this is a background prefetch request.
    pub is_prefetch: bool,
    /// Navigation sequence identifier to discard outdated loads.
    pub sequence: u64,
}

/// A response returned by the persistent loader thread.
pub struct LoaderResponse {
    /// File index inside the App source list.
    pub idx: usize,
    /// The decode result containing the dynamic image, width, height, icon, and file size.
    pub result: Result<(DynamicImage, u32, u32, &'static str, u64), String>,
    /// Whether this was a background prefetch request.
    pub is_prefetch: bool,
    /// Navigation sequence identifier of the load request.
    pub sequence: u64,
    /// Time taken to load and decode.
    pub decode_duration: std::time::Duration,
    /// Whether this response carries a fast-load low-res thumbnail image placeholder.
    pub is_thumbnail: bool,
}

/// Reads the source raw bytes from local files or CBZ zip archives.
pub fn read_source_bytes(source: &ImageSource) -> Result<Vec<u8>, String> {
    match source {
        ImageSource::Local(path) => std::fs::read(path)
            .map_err(|e| format!("Failed to read file:\n{}\n\nError: {}", path.display(), e)),
        ImageSource::Cbz {
            zip_path,
            file_in_zip,
        } => {
            let file = std::fs::File::open(zip_path)
                .map_err(|e| format!("Failed to open zip file {}: {}", zip_path.display(), e))?;
            let reader = std::io::BufReader::new(file);
            let mut archive = zip::ZipArchive::new(reader)
                .map_err(|e| format!("Failed to read zip archive {}: {}", zip_path.display(), e))?;
            let mut zip_entry = archive.by_name(file_in_zip).map_err(|e| {
                format!(
                    "Failed to locate page {} in {}: {}",
                    file_in_zip,
                    zip_path.display(),
                    e
                )
            })?;
            let mut buffer = Vec::with_capacity(zip_entry.size() as usize);
            use std::io::Read;
            zip_entry.read_to_end(&mut buffer).map_err(|e| {
                format!(
                    "Failed to read page data {} from {}: {}",
                    file_in_zip,
                    zip_path.display(),
                    e
                )
            })?;
            Ok(buffer)
        }
    }
}

/// Reads only the first `limit` bytes from local files or CBZ archives.
/// This prevents loading massive files fully into memory when only the EXIF header or thumbnail is needed.
pub fn read_source_bytes_limited(source: &ImageSource, limit: usize) -> Result<Vec<u8>, String> {
    match source {
        ImageSource::Local(path) => {
            use std::io::Read;
            let file = std::fs::File::open(path)
                .map_err(|e| format!("Failed to open file:\n{}\n\nError: {}", path.display(), e))?;
            let mut buffer = Vec::new();
            file.take(limit as u64)
                .read_to_end(&mut buffer)
                .map_err(|e| format!("Failed to read file:\n{}\n\nError: {}", path.display(), e))?;
            Ok(buffer)
        }
        ImageSource::Cbz {
            zip_path,
            file_in_zip,
        } => {
            let file = std::fs::File::open(zip_path)
                .map_err(|e| format!("Failed to open zip file {}: {}", zip_path.display(), e))?;
            let reader = std::io::BufReader::new(file);
            let mut archive = zip::ZipArchive::new(reader)
                .map_err(|e| format!("Failed to read zip archive {}: {}", zip_path.display(), e))?;
            let zip_entry = archive.by_name(file_in_zip).map_err(|e| {
                format!(
                    "Failed to locate page {} in {}: {}",
                    file_in_zip,
                    zip_path.display(),
                    e
                )
            })?;
            let mut buffer = Vec::new();
            use std::io::Read;
            zip_entry
                .take(limit as u64)
                .read_to_end(&mut buffer)
                .map_err(|e| {
                    format!(
                        "Failed to read page data {} from {}: {}",
                        file_in_zip,
                        zip_path.display(),
                        e
                    )
                })?;
            Ok(buffer)
        }
    }
}

/// Decodes an image from already loaded memory bytes.
pub fn decode_image_bytes(
    bytes: &[u8],
    source: &ImageSource,
) -> Result<(DynamicImage, u32, u32, &'static str, u64), String> {
    let file_size = bytes.len() as u64;
    let format = image::guess_format(bytes).ok();
    let display_name = source.display_name();

    if let Some(image::ImageFormat::Jpeg) = format {
        let options = zune_jpeg::zune_core::options::DecoderOptions::default()
            .jpeg_set_out_colorspace(zune_jpeg::zune_core::colorspace::ColorSpace::RGBA);
        let mut decoder = zune_jpeg::JpegDecoder::new_with_options(bytes, options);
        if let Ok(pixels) = decoder.decode()
            && let Some(info) = decoder.info()
            && let Some(rgba_img) =
                image::RgbaImage::from_raw(info.width as u32, info.height as u32, pixels)
        {
            let cursor_meta = std::io::Cursor::new(bytes);
            let orientation = match image::ImageReader::new(cursor_meta).with_guessed_format() {
                Ok(reader) => match reader.into_decoder() {
                    Ok(mut dec) => dec
                        .orientation()
                        .unwrap_or(image::metadata::Orientation::NoTransforms),
                    Err(_) => image::metadata::Orientation::NoTransforms,
                },
                Err(_) => image::metadata::Orientation::NoTransforms,
            };

            let mut img = image::DynamicImage::ImageRgba8(rgba_img);
            img.apply_orientation(orientation);
            let w = img.width();
            let h = img.height();
            return Ok((img, w, h, "\u{F0225}", file_size));
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

    let fmt = reader.format();
    let icon = match fmt {
        Some(image::ImageFormat::Jpeg) => "\u{F0225}",
        Some(image::ImageFormat::Png) => "\u{F0E2D}",
        Some(image::ImageFormat::Gif) => "\u{F0D78}",
        _ => "\u{F021F}",
    };

    let mut decoder = reader
        .into_decoder()
        .map_err(|e| format!("Failed to read metadata:\n{}\n\nError: {}", display_name, e))?;

    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(decoder)
        .map_err(|e| format!("Failed to decode image:\n{}\n\nError: {}", display_name, e))?;

    img.apply_orientation(orientation);
    let rgba_img = img.to_rgba8();
    let w = rgba_img.width();
    let h = rgba_img.height();
    Ok((
        image::DynamicImage::ImageRgba8(rgba_img),
        w,
        h,
        icon,
        file_size,
    ))
}

/// Decodes an image from local paths or comic book archives.
/// Employs zune-jpeg for extremely fast decoding of JPEGs.
pub fn decode_image_source(
    source: ImageSource,
) -> Result<(DynamicImage, u32, u32, &'static str, u64), String> {
    let bytes = read_source_bytes(&source)?;
    decode_image_bytes(&bytes, &source)
}

/// Helper resizing function leveraging fast_image_resize for sub-millisecond execution speeds.
pub fn fast_resize(
    resizer: &mut fir::Resizer,
    src_img: &DynamicImage,
    dst_w: u32,
    dst_h: u32,
    filter_type: FilterType,
    crop_rect: Option<(f64, f64, f64, f64)>,
) -> Result<DynamicImage, Box<dyn std::error::Error>> {
    use fast_image_resize::images::Image as FirImage;

    let resize_alg = match filter_type {
        FilterType::Nearest => fir::ResizeAlg::Nearest,
        FilterType::Triangle => fir::ResizeAlg::Convolution(fir::FilterType::Bilinear),
        FilterType::CatmullRom => fir::ResizeAlg::Convolution(fir::FilterType::CatmullRom),
        FilterType::Mitchell => fir::ResizeAlg::Convolution(fir::FilterType::Mitchell),
        FilterType::Gaussian => fir::ResizeAlg::Convolution(fir::FilterType::Gaussian),
        FilterType::Lanczos3 => fir::ResizeAlg::Convolution(fir::FilterType::Lanczos3),
        FilterType::Hamming => fir::ResizeAlg::Convolution(fir::FilterType::Hamming),
    };

    let temp_rgba;
    let rgba_src = match src_img {
        DynamicImage::ImageRgba8(rgba) => rgba,
        other => {
            temp_rgba = other.to_rgba8();
            &temp_rgba
        }
    };

    let mut dst_image = FirImage::new(dst_w, dst_h, fir::PixelType::U8x4);

    let mut options = fir::ResizeOptions::new();
    options.algorithm = resize_alg;
    if let Some((left, top, width, height)) = crop_rect {
        options = options.crop(left, top, width, height);
    }

    resizer.resize(rgba_src, &mut dst_image, Some(&options))?;

    let buffer = dst_image.into_vec();
    let rgba_dst = image::RgbaImage::from_raw(dst_w, dst_h, buffer)
        .ok_or("Failed to construct RgbaImage from resized buffer")?;
    Ok(DynamicImage::ImageRgba8(rgba_dst))
}

/// Processes a scaling and panning request in the background, creating/rendering
/// the final scaled viewport on a screen-pixel canvas block to support offscreen panning boundaries.
pub fn process_resize(
    req: ResizeRequest,
    resizer: &mut fir::Resizer,
) -> (StatefulProtocol, std::time::Duration, std::time::Duration) {
    let start_process = std::time::Instant::now();

    // Map input crop/intersection coordinates from the full image space to the thumbnail space
    // if the loaded image buffer is a thumbnail placeholder of different dimensions.
    let (img_to_resize, scale, crop, intersection) =
        if req.img.width() != req.original_size.0 || req.img.height() != req.original_size.1 {
            let factor_x = req.img.width() as f64 / req.original_size.0 as f64;
            let factor_y = req.img.height() as f64 / req.original_size.1 as f64;

            let crop_x1 = (req.crop.x1 as f64 * factor_x).round() as i64;
            let crop_y1 = (req.crop.y1 as f64 * factor_y).round() as i64;
            let crop_x2 = (req.crop.x2 as f64 * factor_x).round() as i64;
            let crop_y2 = (req.crop.y2 as f64 * factor_y).round() as i64;
            let scaled_crop = CropBox::new(crop_x1, crop_y1, crop_x2, crop_y2);

            let inter_x1 =
                ((req.intersection.x1 as f64 * factor_x).round() as u32).min(req.img.width());
            let inter_y1 =
                ((req.intersection.y1 as f64 * factor_y).round() as u32).min(req.img.height());
            let inter_x2 =
                ((req.intersection.x2 as f64 * factor_x).round() as u32).min(req.img.width());
            let inter_y2 =
                ((req.intersection.y2 as f64 * factor_y).round() as u32).min(req.img.height());
            let scaled_inter = ImageIntersection::new(inter_x1, inter_y1, inter_x2, inter_y2);

            let new_scale = req.target_w as f64 / scaled_crop.width().max(1) as f64;

            (Arc::clone(&req.img), new_scale, scaled_crop, scaled_inter)
        } else {
            (Arc::clone(&req.img), req.scale, req.crop, req.intersection)
        };

    let mut canvas = if intersection.x1 as i64 == crop.x1
        && intersection.x2 as i64 == crop.x2
        && intersection.y1 as i64 == crop.y1
        && intersection.y2 as i64 == crop.y2
    {
        let crop_rect = Some((
            intersection.x1 as f64,
            intersection.y1 as f64,
            intersection.width() as f64,
            intersection.height() as f64,
        ));
        match fast_resize(
            resizer,
            &img_to_resize,
            req.target_w,
            req.target_h,
            req.filter_type,
            crop_rect,
        ) {
            Ok(resized) => resized,
            Err(_) => {
                let cropped_part = img_to_resize.crop_imm(
                    intersection.x1,
                    intersection.y1,
                    intersection.width(),
                    intersection.height(),
                );
                cropped_part.resize(
                    req.target_w,
                    req.target_h,
                    req.filter_type.to_image_filter(),
                )
            }
        }
    } else {
        let mut screen_canvas = image::RgbaImage::new(req.target_w, req.target_h);

        if !intersection.is_empty() {
            let target_inter_w = ((intersection.width() as f64 * scale).round() as u32).max(1);
            let target_inter_h = ((intersection.height() as f64 * scale).round() as u32).max(1);

            let crop_rect = Some((
                intersection.x1 as f64,
                intersection.y1 as f64,
                intersection.width() as f64,
                intersection.height() as f64,
            ));

            let resized_part = match fast_resize(
                resizer,
                &img_to_resize,
                target_inter_w,
                target_inter_h,
                req.filter_type,
                crop_rect,
            ) {
                Ok(resized) => resized,
                Err(_) => {
                    let cropped_part = img_to_resize.crop_imm(
                        intersection.x1,
                        intersection.y1,
                        intersection.width(),
                        intersection.height(),
                    );
                    cropped_part.resize(
                        target_inter_w,
                        target_inter_h,
                        req.filter_type.to_image_filter(),
                    )
                }
            };

            let paste_x = ((intersection.x1 as i64 - crop.x1) as f64 * scale).round() as i64;
            let paste_y = ((intersection.y1 as i64 - crop.y1) as f64 * scale).round() as i64;

            let paste_x =
                paste_x.clamp(0, (req.target_w as i64 - target_inter_w as i64).max(0)) as u32;
            let paste_y =
                paste_y.clamp(0, (req.target_h as i64 - target_inter_h as i64).max(0)) as u32;

            let copy_w = target_inter_w.min(req.target_w.saturating_sub(paste_x));
            let copy_h = target_inter_h.min(req.target_h.saturating_sub(paste_y));

            if copy_w > 0 && copy_h > 0 {
                let part_to_copy = if copy_w < target_inter_w || copy_h < target_inter_h {
                    resized_part.crop_imm(0, 0, copy_w, copy_h)
                } else {
                    resized_part
                };

                if let Some(rgba_part) = part_to_copy.as_rgba8() {
                    let _ = screen_canvas.copy_from(rgba_part, paste_x, paste_y);
                } else {
                    let _ = screen_canvas.copy_from(&part_to_copy.to_rgba8(), paste_x, paste_y);
                }
            }
        }
        DynamicImage::ImageRgba8(screen_canvas)
    };

    if let Some(rgba_canvas) = canvas.as_mut_rgba8() {
        if req.brightness.value() != 0 {
            image::imageops::colorops::brighten_in_place(rgba_canvas, req.brightness.value());
        }
        if req.contrast.value() != 0.0 {
            image::imageops::colorops::contrast_in_place(rgba_canvas, req.contrast.value());
        }
    }
    let process_duration = start_process.elapsed();

    let start_protocol = std::time::Instant::now();
    let protocol = req.picker.new_resize_protocol(canvas);
    let protocol_duration = start_protocol.elapsed();

    (protocol, process_duration, protocol_duration)
}

/// Scans the directory surrounding the initial path, returns sorted paths of
/// supported images along with the index pointing to the initial file.
pub fn scan_directory(
    initial_path: &Path,
    check_magic: bool,
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
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if is_image_file(&path, check_magic) {
                images.push(path);
            }
        }
    }

    images.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

    let index = if let Some(ref target_name) = file_name {
        images
            .iter()
            .position(|path| path.file_name().map(|n| n == target_name).unwrap_or(false))
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
    if let Some(ext) = path.extension().and_then(|e| e.to_str())
        && (ext.eq_ignore_ascii_case("png")
            || ext.eq_ignore_ascii_case("jpg")
            || ext.eq_ignore_ascii_case("jpeg")
            || ext.eq_ignore_ascii_case("gif")
            || ext.eq_ignore_ascii_case("webp")
            || ext.eq_ignore_ascii_case("bmp")
            || ext.eq_ignore_ascii_case("tiff")
            || ext.eq_ignore_ascii_case("ico"))
    {
        return true;
    }
    if check_magic {
        matches!(guess_file_type(path), Some(GuessType::Image))
    } else {
        false
    }
}

/// Helper to determine if a file is a comic book archive (CBZ) or generic ZIP.
pub fn is_cbz_or_zip(path: &Path, check_magic: bool) -> bool {
    if !path.is_file() {
        return false;
    }
    if let Some(ext) = path.extension().and_then(|e| e.to_str())
        && (ext.eq_ignore_ascii_case("cbz") || ext.eq_ignore_ascii_case("zip"))
    {
        return true;
    }
    if check_magic {
        matches!(guess_file_type(path), Some(GuessType::Zip))
    } else {
        false
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
            let pages = list_cbz_pages(path)?;
            for page in pages {
                sources.push(ImageSource::Cbz {
                    zip_path: path.clone(),
                    file_in_zip: page,
                });
            }
        } else if is_image_file(path, check_magic) {
            sources.push(ImageSource::Local(path.clone()));
        }
    }
    Ok(sources)
}

/// Tries to extract the embedded JPEG thumbnail from EXIF APP1 metadata segment.
pub fn extract_jpeg_thumbnail(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut cursor = std::io::Cursor::new(bytes);
    let exif = exif::Reader::new().read_from_container(&mut cursor).ok()?;

    let offset = exif
        .get_field(exif::Tag::JPEGInterchangeFormat, exif::In::THUMBNAIL)
        .and_then(|f| match f.value {
            exif::Value::Long(ref v) => v.first().copied(),
            _ => None,
        })? as usize;

    let length = exif
        .get_field(exif::Tag::JPEGInterchangeFormatLength, exif::In::THUMBNAIL)
        .and_then(|f| match f.value {
            exif::Value::Long(ref v) => v.first().copied(),
            _ => None,
        })? as usize;

    let tiff_start = bytes.windows(6).position(|window| window == b"Exif\0\0")? + 6;

    let end = tiff_start
        .checked_add(offset)
        .and_then(|val| val.checked_add(length))?;

    if end <= bytes.len() {
        Some(bytes[tiff_start + offset..end].to_vec())
    } else {
        None
    }
}

/// Reads the image dimensions extremely quickly from header headers, and decodes/scales
/// its embedded EXIF thumbnail to serve as a fast low-res placeholder for large cold loads.
pub fn decode_thumbnail_and_dimensions(bytes: &[u8]) -> Option<(DynamicImage, u32, u32)> {
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
    let (real_w, real_h) = if swaps {
        (raw_h, raw_w)
    } else {
        (raw_w, raw_h)
    };

    // Crop out any black padding/letterboxing in the thumbnail frame
    // caused by aspect ratio differences between the thumbnail bounding box and the main image.
    let ar_main = real_w as f64 / real_h as f64;
    let thumb_w = thumb_img.width();
    let thumb_h = thumb_img.height();
    let ar_thumb = thumb_w as f64 / thumb_h as f64;

    let cropped_thumb = if (ar_main - ar_thumb).abs() > 0.01 {
        if ar_main > ar_thumb {
            // Main image is wider than thumbnail box: crop top/bottom black bars
            let content_h = (thumb_w as f64 / ar_main).round() as u32;
            let padding_y = (thumb_h.saturating_sub(content_h)) / 2;
            let content_h = content_h.min(thumb_h.saturating_sub(padding_y));
            if content_h > 0 {
                thumb_img.crop_imm(0, padding_y, thumb_w, content_h)
            } else {
                thumb_img
            }
        } else {
            // Main image is taller than thumbnail box: crop left/right black bars
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

    // Return the tiny oriented and cropped thumbnail directly without upscaling to avoid massive buffers/CPU overhead
    Some((cropped_thumb, real_w, real_h))
}
