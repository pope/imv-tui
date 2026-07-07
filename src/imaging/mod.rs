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

/// Encapsulates success values for a decoded image.
pub struct DecodedImage {
    /// The decoded DynamicImage.
    pub image: DynamicImage,
    /// Width of the image in pixels.
    pub width: u32,
    /// Height of the image in pixels.
    pub height: u32,
    /// Optional format parsed during load.
    pub format: Option<image::ImageFormat>,
    /// The size of the raw source file in bytes.
    pub disk_size: u64,
}

/// A response returned by the persistent loader thread.
pub struct LoaderResponse {
    /// File index inside the App source list.
    pub idx: usize,
    /// The decode result containing the DecodedImage data.
    pub result: Result<DecodedImage, String>,
    /// Whether this was a background prefetch request.
    pub is_prefetch: bool,
    /// Navigation sequence identifier of the load request.
    pub sequence: u64,
    /// Time taken to load and decode.
    pub decode_duration: std::time::Duration,
    /// Whether this response carries a fast-load low-res thumbnail image placeholder.
    pub is_thumbnail: bool,
}

fn read_source_range(source: &ImageSource, offset: u64, length: u64) -> Result<Vec<u8>, String> {
    if length > 64 * 1024 * 1024 {
        return Err(format!(
            "Requested read segment length {} is too large (max 64MB)",
            length
        ));
    }
    match source {
        ImageSource::Local(path) => {
            use std::io::{Read, Seek};
            let mut file =
                std::fs::File::open(path).map_err(|e| format!("Failed to open file: {}", e))?;
            file.seek(std::io::SeekFrom::Start(offset))
                .map_err(|e| format!("Failed to seek file: {}", e))?;
            let mut buffer = vec![0u8; length as usize];
            file.read_exact(&mut buffer)
                .map_err(|e| format!("Failed to read file segment: {}", e))?;
            Ok(buffer)
        }
        ImageSource::Cbz {
            zip_path,
            file_in_zip,
        } => {
            let file = std::fs::File::open(zip_path)
                .map_err(|e| format!("Failed to open zip file: {}", e))?;
            let reader = std::io::BufReader::new(file);
            let mut archive = zip::ZipArchive::new(reader)
                .map_err(|e| format!("Failed to read zip archive: {}", e))?;
            let mut zip_entry = archive
                .by_name(file_in_zip)
                .map_err(|e| format!("Failed to locate zip entry: {}", e))?;
            let _total_len = offset
                .checked_add(length)
                .ok_or_else(|| "Offset + length calculation overflowed u64".to_string())?;
            use std::io::Read;
            std::io::copy(&mut zip_entry.by_ref().take(offset), &mut std::io::sink())
                .map_err(|e| format!("Failed to seek zip entry offset: {}", e))?;
            let mut buffer = vec![0u8; length as usize];
            zip_entry
                .read_exact(&mut buffer)
                .map_err(|e| format!("Failed to read zip entry segment: {}", e))?;
            Ok(buffer)
        }
    }
}

fn read_raw_preview_bytes(source: &ImageSource) -> Option<Vec<u8>> {
    let header_bytes = read_source_bytes_limited(source, 256 * 1024).ok()?;

    if header_bytes.starts_with(b"FUJIFILM") {
        if header_bytes.len() < 92 {
            return None;
        }
        let offset = u32::from_be_bytes(header_bytes[84..88].try_into().ok()?) as u64;
        let length = u32::from_be_bytes(header_bytes[88..92].try_into().ok()?) as u64;
        return read_source_range(source, offset, length).ok();
    }

    let mut cursor = std::io::Cursor::new(&header_bytes);
    let exif = exif::Reader::new().read_from_container(&mut cursor).ok()?;

    let primary_offset = exif.get_field(exif::Tag::JPEGInterchangeFormat, exif::In::PRIMARY);
    let primary_length = exif.get_field(exif::Tag::JPEGInterchangeFormatLength, exif::In::PRIMARY);

    let thumb_offset = exif.get_field(exif::Tag::JPEGInterchangeFormat, exif::In::THUMBNAIL);
    let thumb_length = exif.get_field(exif::Tag::JPEGInterchangeFormatLength, exif::In::THUMBNAIL);

    let mut best_offset = None;
    let mut best_length = 0;

    if let Some(f_off) = primary_offset
        && let Some(f_len) = primary_length
    {
        let off = match f_off.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        };
        let len = match f_len.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        };
        if let Some(off) = off
            && let Some(len) = len
        {
            best_offset = Some(off as u64);
            best_length = len as u64;
        }
    }

    if let Some(f_off) = thumb_offset
        && let Some(f_len) = thumb_length
    {
        let off = match f_off.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        };
        let len = match f_len.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        };
        if let Some(off) = off
            && let Some(len) = len
            && len as u64 > best_length
        {
            best_offset = Some(off as u64);
            best_length = len as u64;
        }
    }

    let offset = best_offset?;
    let length = best_length;

    let is_tiff = header_bytes.len() >= 4
        && ((header_bytes[0] == 0x49
            && header_bytes[1] == 0x49
            && header_bytes[2] == 0x2A
            && header_bytes[3] == 0x00)
            || (header_bytes[0] == 0x4D
                && header_bytes[1] == 0x4D
                && header_bytes[2] == 0x00
                && header_bytes[3] == 0x2A));

    let tiff_start = if is_tiff {
        0
    } else {
        header_bytes
            .windows(6)
            .position(|window| window == b"Exif\0\0")? as u64
            + 6
    };

    let start = tiff_start.checked_add(offset)?;
    read_source_range(source, start, length).ok()
}

/// Reads the source raw bytes from local files or CBZ zip archives.
pub fn read_source_bytes(source: &ImageSource) -> Result<Vec<u8>, String> {
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

    if is_raw_extension && let Some(preview_bytes) = read_raw_preview_bytes(source) {
        return Ok(preview_bytes);
    }

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

    if format != Some(image::ImageFormat::Jpeg)
        && (format == Some(image::ImageFormat::Tiff) || is_raw_extension)
        && let Some(jpeg_bytes) = extract_jpeg_preview(bytes)
    {
        // Recurse using the extracted JPEG bytes
        if let Ok(mut decoded) = decode_image_bytes(&jpeg_bytes, source) {
            decoded.format = Some(image::ImageFormat::Jpeg);
            decoded.disk_size = file_size;
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
            return Ok(DecodedImage {
                image: img,
                width: w,
                height: h,
                format: Some(image::ImageFormat::Jpeg),
                disk_size: file_size,
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
    Ok(DecodedImage {
        image: image::DynamicImage::ImageRgba8(rgba_img),
        width: w,
        height: h,
        format,
        disk_size: file_size,
    })
}

/// Decodes an image from local paths or comic book archives.
/// Employs zune-jpeg for extremely fast decoding of JPEGs.
pub fn decode_image_source(source: ImageSource) -> Result<DecodedImage, String> {
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
        FilterType::Linear => fir::ResizeAlg::Convolution(fir::FilterType::Bilinear),
        FilterType::Cubic => fir::ResizeAlg::Convolution(fir::FilterType::CatmullRom),
        FilterType::Mitchell => fir::ResizeAlg::Convolution(fir::FilterType::Mitchell),
        FilterType::Gaussian => fir::ResizeAlg::Convolution(fir::FilterType::Gaussian),
        FilterType::Lanczos => fir::ResizeAlg::Convolution(fir::FilterType::Lanczos3),
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
        if !req.brightness.is_zero() {
            image::imageops::colorops::brighten_in_place(rgba_canvas, req.brightness.value());
        }
        if !req.contrast.is_zero() {
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

/// Helper to extract EXIF orientation from raw bytes.
fn get_exif_orientation(bytes: &[u8]) -> image::metadata::Orientation {
    let mut cursor = std::io::Cursor::new(bytes);
    let exif = match exif::Reader::new().read_from_container(&mut cursor) {
        Ok(exif) => exif,
        Err(_) => return image::metadata::Orientation::NoTransforms,
    };
    let val = exif
        .get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|f| match f.value {
            exif::Value::Short(ref v) => v.first().copied(),
            _ => None,
        })
        .unwrap_or(1);
    match val {
        1 => image::metadata::Orientation::NoTransforms,
        2 => image::metadata::Orientation::FlipHorizontal,
        3 => image::metadata::Orientation::Rotate180,
        4 => image::metadata::Orientation::FlipVertical,
        5 => image::metadata::Orientation::Rotate90FlipH,
        6 => image::metadata::Orientation::Rotate90,
        7 => image::metadata::Orientation::Rotate270FlipH,
        8 => image::metadata::Orientation::Rotate270,
        _ => image::metadata::Orientation::NoTransforms,
    }
}

/// Tries to extract the smaller embedded JPEG thumbnail from EXIF APP1 metadata segment or TIFF/RAW container.
pub fn extract_jpeg_thumbnail(bytes: &[u8]) -> Option<Vec<u8>> {
    // 1. Detect Fujifilm RAF raw formats (which are not standard TIFF files)
    if bytes.starts_with(b"FUJIFILM") {
        if bytes.len() < 92 {
            return None;
        }
        let offset = u32::from_be_bytes(bytes[84..88].try_into().ok()?) as usize;
        let length = u32::from_be_bytes(bytes[88..92].try_into().ok()?) as usize;
        let end = offset.checked_add(length)?;
        if end <= bytes.len() {
            return Some(bytes[offset..end].to_vec());
        }
        return None;
    }

    // 2. Parse standard TIFF/EXIF structures for other formats
    let mut cursor = std::io::Cursor::new(bytes);
    let exif = exif::Reader::new().read_from_container(&mut cursor).ok()?;

    let primary_offset = exif.get_field(exif::Tag::JPEGInterchangeFormat, exif::In::PRIMARY);
    let primary_length = exif.get_field(exif::Tag::JPEGInterchangeFormatLength, exif::In::PRIMARY);

    let thumb_offset = exif.get_field(exif::Tag::JPEGInterchangeFormat, exif::In::THUMBNAIL);
    let thumb_length = exif.get_field(exif::Tag::JPEGInterchangeFormatLength, exif::In::THUMBNAIL);

    let mut candidates = Vec::new();

    if let Some(f_off) = primary_offset
        && let Some(f_len) = primary_length
    {
        let off = match f_off.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        };
        let len = match f_len.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        };
        if let Some(off) = off
            && let Some(len) = len
        {
            candidates.push((off as usize, len as usize));
        }
    }

    if let Some(f_off) = thumb_offset
        && let Some(f_len) = thumb_length
    {
        let off = match f_off.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        };
        let len = match f_len.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        };
        if let Some(off) = off
            && let Some(len) = len
        {
            candidates.push((off as usize, len as usize));
        }
    }

    // Sort by length ascending (smallest first)
    candidates.sort_by_key(|&(_, len)| len);

    // TIFF magic headers: II*\0 or MM\0*
    let is_tiff = bytes.len() >= 4
        && ((bytes[0] == 0x49 && bytes[1] == 0x49 && bytes[2] == 0x2A && bytes[3] == 0x00)
            || (bytes[0] == 0x4D && bytes[1] == 0x4D && bytes[2] == 0x00 && bytes[3] == 0x2A));

    let tiff_start = if is_tiff {
        0
    } else {
        bytes.windows(6).position(|window| window == b"Exif\0\0")? + 6
    };

    // Pick the smallest candidate that fits within the provided bytes slice
    for (offset, length) in candidates {
        if let Some(start) = tiff_start.checked_add(offset)
            && let Some(end) = start.checked_add(length)
            && end <= bytes.len()
        {
            return Some(bytes[start..end].to_vec());
        }
    }

    None
}

/// Tries to extract the larger/highest-resolution embedded JPEG preview from EXIF APP1 metadata segment or TIFF/RAW/RAF container.
pub fn extract_jpeg_preview(bytes: &[u8]) -> Option<Vec<u8>> {
    // 1. Detect Fujifilm RAF raw formats (which are not standard TIFF files)
    if bytes.starts_with(b"FUJIFILM") {
        if bytes.len() < 92 {
            return None;
        }
        let offset = u32::from_be_bytes(bytes[84..88].try_into().ok()?) as usize;
        let length = u32::from_be_bytes(bytes[88..92].try_into().ok()?) as usize;
        let end = offset.checked_add(length)?;
        if end <= bytes.len() {
            return Some(bytes[offset..end].to_vec());
        }
        return None;
    }

    // 2. Parse standard TIFF/EXIF structures for other formats
    let mut cursor = std::io::Cursor::new(bytes);
    let exif = exif::Reader::new().read_from_container(&mut cursor).ok()?;

    let primary_offset = exif.get_field(exif::Tag::JPEGInterchangeFormat, exif::In::PRIMARY);
    let primary_length = exif.get_field(exif::Tag::JPEGInterchangeFormatLength, exif::In::PRIMARY);

    let thumb_offset = exif.get_field(exif::Tag::JPEGInterchangeFormat, exif::In::THUMBNAIL);
    let thumb_length = exif.get_field(exif::Tag::JPEGInterchangeFormatLength, exif::In::THUMBNAIL);

    let mut best_offset = None;
    let mut best_length = 0;

    if let Some(f_off) = primary_offset
        && let Some(f_len) = primary_length
    {
        let off = match f_off.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        };
        let len = match f_len.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        };
        if let Some(off) = off
            && let Some(len) = len
        {
            best_offset = Some(off as usize);
            best_length = len as usize;
        }
    }

    if let Some(f_off) = thumb_offset
        && let Some(f_len) = thumb_length
    {
        let off = match f_off.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        };
        let len = match f_len.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        };
        if let Some(off) = off
            && let Some(len) = len
            && len as usize > best_length
        {
            best_offset = Some(off as usize);
            best_length = len as usize;
        }
    }

    let offset = best_offset?;
    let length = best_length;

    // TIFF magic headers: II*\0 or MM\0*
    let is_tiff = bytes.len() >= 4
        && ((bytes[0] == 0x49 && bytes[1] == 0x49 && bytes[2] == 0x2A && bytes[3] == 0x00)
            || (bytes[0] == 0x4D && bytes[1] == 0x4D && bytes[2] == 0x00 && bytes[3] == 0x2A));

    let tiff_start = if is_tiff {
        0
    } else {
        bytes.windows(6).position(|window| window == b"Exif\0\0")? + 6
    };

    let start = tiff_start.checked_add(offset)?;
    let end = start.checked_add(length)?;

    if end <= bytes.len() {
        Some(bytes[start..end].to_vec())
    } else {
        None
    }
}

fn get_dimensions_from_exif(exif: &exif::Exif) -> Option<(u32, u32)> {
    let w = exif
        .get_field(exif::Tag::ImageWidth, exif::In::PRIMARY)
        .or_else(|| exif.get_field(exif::Tag::PixelXDimension, exif::In::PRIMARY))
        .and_then(|f| match f.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        })?;

    let h = exif
        .get_field(exif::Tag::ImageLength, exif::In::PRIMARY)
        .or_else(|| exif.get_field(exif::Tag::PixelYDimension, exif::In::PRIMARY))
        .and_then(|f| match f.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        })?;

    Some((w, h))
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

    Some((cropped_thumb, real_w, real_h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn test_is_image_file_logic() {
        let _ = fs::create_dir_all("target/tmp");
        let txt_path = PathBuf::from("target/tmp/fake_image.png");
        fs::write(&txt_path, "not an image").unwrap();

        // Without check_magic, it matches by extension .png
        assert!(is_image_file(&txt_path, false));

        // With check_magic, it validates bytes and rejects it
        assert!(!is_image_file(&txt_path, true));

        let _ = fs::remove_file(txt_path);
    }

    #[test]
    fn test_recursive_directory_scan() {
        let _ = fs::create_dir_all("target/tmp/scan_test/sub");
        let img1 = PathBuf::from("target/tmp/scan_test/img1.png");
        let img2 = PathBuf::from("target/tmp/scan_test/sub/img2.jpg");
        fs::write(&img1, "fake").unwrap();
        fs::write(&img2, "fake").unwrap();

        // Scan non-recursively
        let (non_rec_files, _) =
            scan_directory(&PathBuf::from("target/tmp/scan_test"), false, false).unwrap();
        assert_eq!(non_rec_files.len(), 1);
        assert_eq!(non_rec_files[0].file_name().unwrap(), "img1.png");

        // Scan recursively
        let (rec_files, _) =
            scan_directory(&PathBuf::from("target/tmp/scan_test"), false, true).unwrap();
        assert_eq!(rec_files.len(), 2);

        let _ = fs::remove_dir_all("target/tmp/scan_test");

        // Verify file path deduplication (e.g. symlinks pointing to the same file)
        #[cfg(unix)]
        {
            let _ = fs::create_dir_all("target/tmp/scan_test");
            let img1 = PathBuf::from("target/tmp/scan_test/img1.png");
            let sym = PathBuf::from("target/tmp/scan_test/symlink_img1.png");
            fs::write(&img1, "fake").unwrap();
            let _ = std::os::unix::fs::symlink(&img1, &sym);

            let (rec_files, _) =
                scan_directory(&PathBuf::from("target/tmp/scan_test"), false, true).unwrap();
            assert_eq!(rec_files.len(), 1);

            let _ = fs::remove_dir_all("target/tmp/scan_test");
        }
    }

    #[test]
    fn test_magic_bytes_guessing() {
        let dir = PathBuf::from("target/tmp/magic_test");
        let _ = fs::create_dir_all(&dir);

        let jpg_path = dir.join("test.jpg");
        fs::write(&jpg_path, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46]).unwrap();

        let png_path = dir.join("test.png");
        fs::write(&png_path, [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]).unwrap();

        let zip_path = dir.join("test.zip");
        fs::write(&zip_path, [0x50, 0x4B, 0x03, 0x04, 0x0A, 0x00, 0x00, 0x00]).unwrap();

        let txt_path = dir.join("test.txt");
        fs::write(&txt_path, b"hello world this is a text file").unwrap();

        // Test guess_file_type
        assert_eq!(guess_file_type(&jpg_path), Some(GuessType::Image));
        assert_eq!(guess_file_type(&png_path), Some(GuessType::Image));
        assert_eq!(guess_file_type(&zip_path), Some(GuessType::Zip));
        assert_eq!(guess_file_type(&txt_path), None);

        // Test is_image_file with check_magic = true
        assert!(is_image_file(&jpg_path, true));
        assert!(is_image_file(&png_path, true));
        assert!(!is_image_file(&zip_path, true));
        assert!(!is_image_file(&txt_path, true));

        // Test is_cbz_or_zip with check_magic = true
        assert!(is_cbz_or_zip(&zip_path, true));
        assert!(!is_cbz_or_zip(&jpg_path, true));
        assert!(!is_cbz_or_zip(&txt_path, true));

        // Test with check_magic = false (extension-only)
        assert!(is_image_file(&jpg_path, false));
        assert!(is_cbz_or_zip(&zip_path, false));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_cbz_page_decoding() {
        let zip_path = PathBuf::from("target/tmp/test_cbz.cbz");
        let _ = fs::create_dir_all("target/tmp");

        {
            let file = fs::File::create(&zip_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::FileOptions::<()>::default();

            zip.start_file("cover.png", options).unwrap();
            use std::io::Write;
            zip.write_all(b"fake png data").unwrap();

            zip.start_file("page1.jpg", options).unwrap();
            zip.write_all(b"fake jpg data").unwrap();

            zip.start_file("README.txt", options).unwrap();
            zip.write_all(b"text file").unwrap();

            zip.start_file("subfolder/page2.webp", options).unwrap();
            zip.write_all(b"fake webp data").unwrap();

            zip.finish().unwrap();
        }

        // Test list_cbz_pages
        let pages = list_cbz_pages(&zip_path).unwrap();
        assert_eq!(pages.len(), 3);
        assert_eq!(pages[0], "cover.png");
        assert_eq!(pages[1], "page1.jpg");
        assert_eq!(pages[2], "subfolder/page2.webp");

        // Test read_source_bytes_limited from inside CBZ
        let src = ImageSource::Cbz {
            zip_path: zip_path.clone(),
            file_in_zip: "cover.png".to_string(),
        };
        let bytes_limited = read_source_bytes_limited(&src, 4).unwrap();
        assert_eq!(bytes_limited, b"fake");

        let bytes_all = read_source_bytes(&src).unwrap();
        assert_eq!(bytes_all, b"fake png data");

        // Test collect_sources
        let sources = collect_sources(std::slice::from_ref(&zip_path), true).unwrap();
        assert_eq!(sources.len(), 3);
        match &sources[0] {
            ImageSource::Cbz { file_in_zip, .. } => assert_eq!(file_in_zip, "cover.png"),
            _ => panic!("Expected Cbz source"),
        }

        let _ = fs::remove_file(zip_path);
    }

    #[test]
    fn test_read_source_bytes_limited_local() {
        let dir = PathBuf::from("target/tmp/limited_test");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("test.dat");
        fs::write(&path, b"0123456789").unwrap();

        let src = ImageSource::Local(path.clone());
        let limited = read_source_bytes_limited(&src, 5).unwrap();
        assert_eq!(limited, b"01234");

        let unlimited = read_source_bytes_limited(&src, 50).unwrap();
        assert_eq!(unlimited, b"0123456789");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_empty_or_corrupt_exif() {
        let empty_bytes = &[];
        assert_eq!(
            get_exif_orientation(empty_bytes),
            image::metadata::Orientation::NoTransforms
        );
        assert!(extract_jpeg_thumbnail(empty_bytes).is_none());

        let corrupt_bytes = &[0x01, 0x02, 0x03, 0x04];
        assert_eq!(
            get_exif_orientation(corrupt_bytes),
            image::metadata::Orientation::NoTransforms
        );
        assert!(extract_jpeg_thumbnail(corrupt_bytes).is_none());
    }

    #[test]
    fn test_raw_image_file_recognition() {
        // Assert RAW extensions are recognized as valid image files
        let dir = PathBuf::from("target/tmp/raw_test");
        let _ = fs::create_dir_all(&dir);

        let dng_file = dir.join("test.dng");
        let nef_file = dir.join("test.nef");
        let cr2_file = dir.join("test.cr2");
        let arw_file = dir.join("test.arw");

        fs::write(&dng_file, []).unwrap();
        fs::write(&nef_file, []).unwrap();
        fs::write(&cr2_file, []).unwrap();
        fs::write(&arw_file, []).unwrap();

        assert!(is_image_file(&dng_file, false));
        assert!(is_image_file(&nef_file, false));
        assert!(is_image_file(&cr2_file, false));
        assert!(is_image_file(&arw_file, false));

        assert!(!is_cbz_or_zip(&dng_file, false));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_raf_preview_extraction() {
        let mut raf_bytes = vec![0u8; 100];
        // Write Fuji magic signature
        raf_bytes[0..8].copy_from_slice(b"FUJIFILM");

        // Write JPEG offset = 92
        let offset = 92u32.to_be_bytes();
        raf_bytes[84..88].copy_from_slice(&offset);

        // Write JPEG length = 4
        let length = 4u32.to_be_bytes();
        raf_bytes[88..92].copy_from_slice(&length);

        // Write mock JPEG bytes
        raf_bytes[92..96].copy_from_slice(b"JPEG");

        let extracted = extract_jpeg_thumbnail(&raf_bytes).unwrap();
        assert_eq!(extracted, b"JPEG");
    }

    #[test]
    fn test_arw_diagnostic() {
        let path =
            PathBuf::from("/home/pope/Pictures/2017-08-16.KayJay/Capture/20170816-KayJay-0121.ARW");
        if path.exists() {
            let bytes = std::fs::read(&path).unwrap();
            let partial = &bytes[..256 * 1024];
            println!("ARW file size: {}", bytes.len());

            let mut cursor = std::io::Cursor::new(partial);
            let exif = exif::Reader::new().read_from_container(&mut cursor);
            match exif {
                Ok(exif) => {
                    println!("EXIF successfully parsed!");
                    for f in exif.fields() {
                        if f.tag == exif::Tag::JPEGInterchangeFormat
                            || f.tag == exif::Tag::JPEGInterchangeFormatLength
                        {
                            println!(
                                "Tag: {:?}, IFD: {:?}, Value: {:?}",
                                f.tag, f.ifd_num, f.value
                            );
                        }
                    }
                    let thumb = extract_jpeg_thumbnail(partial);
                    println!("extracted thumbnail size: {:?}", thumb.map(|t| t.len()));

                    let thumb_and_dims = decode_thumbnail_and_dimensions(partial);
                    assert!(thumb_and_dims.is_some());
                    let (_, w, h) = thumb_and_dims.unwrap();
                    println!("Parsed RAW dimensions: {}x{}", w, h);
                    assert!(w > 0 && h > 0);
                }
                Err(e) => {
                    println!("EXIF parse error: {:?}", e);
                }
            }
        }
    }

    #[test]
    fn test_read_source_range_limits() {
        let path = PathBuf::from("target/tmp/nonexistent_file_limits.dat");
        let src = ImageSource::Local(path);
        // length > 64MB should fail with size limit error immediately
        let res = read_source_range(&src, 0, 65 * 1024 * 1024);
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("too large"));

        // offset + length overflow should fail immediately in Cbz
        let cbz_src = ImageSource::Cbz {
            zip_path: PathBuf::from("target/tmp/nonexistent.zip"),
            file_in_zip: "test.png".to_string(),
        };
        let res = read_source_range(&cbz_src, u64::MAX - 10, 20);
        assert!(res.is_err());
    }

    #[test]
    fn test_cbz_read_source_range_streaming() {
        let zip_path = PathBuf::from("target/tmp/test_cbz_stream.cbz");
        let _ = fs::create_dir_all("target/tmp");

        {
            let file = fs::File::create(&zip_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::FileOptions::<()>::default();

            zip.start_file("data.bin", options).unwrap();
            use std::io::Write;
            zip.write_all(b"0123456789abcdef").unwrap();
            zip.finish().unwrap();
        }

        let src = ImageSource::Cbz {
            zip_path: zip_path.clone(),
            file_in_zip: "data.bin".to_string(),
        };

        // Read offset=4, length=4 (should return "4567")
        let res = read_source_range(&src, 4, 4).unwrap();
        assert_eq!(res, b"4567");

        // Read out of bounds should error gracefully
        let res = read_source_range(&src, 10, 10);
        assert!(res.is_err());

        let _ = fs::remove_file(zip_path);
    }
}
