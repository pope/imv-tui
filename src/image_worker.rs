use fast_image_resize as fir;
use image::{DynamicImage, GenericImage, ImageDecoder};
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Resizing filter types for scaling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterType {
    /// Nearest neighbor.
    Nearest,
    /// Linear interpolation.
    Triangle,
    /// Cubic spline filter.
    CatmullRom,
    /// Mitchell Netravali.
    Mitchell,
    /// Gaussian filter.
    Gaussian,
    /// Lanczos windowed sinc.
    Lanczos3,
    /// Hamming filter.
    Hamming,
}

impl FilterType {
    /// Maps our `FilterType` variants to the `image::imageops::FilterType` counterparts.
    pub fn to_image_filter(self) -> image::imageops::FilterType {
        match self {
            FilterType::Nearest => image::imageops::FilterType::Nearest,
            FilterType::Triangle => image::imageops::FilterType::Triangle,
            FilterType::CatmullRom => image::imageops::FilterType::CatmullRom,
            FilterType::Mitchell => image::imageops::FilterType::CatmullRom,
            FilterType::Gaussian => image::imageops::FilterType::Gaussian,
            FilterType::Lanczos3 => image::imageops::FilterType::Lanczos3,
            FilterType::Hamming => image::imageops::FilterType::Triangle,
        }
    }
}

/// Zoom/scale modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleMode {
    /// Show image at 1:1 original pixels.
    None,
    /// Scale larger images down to fit, leave smaller untouched.
    Shrink,
    /// Stretch/shrink to perfectly match viewport size.
    Full,
    /// Crop to cover entire viewport.
    Crop,
}

impl ScaleMode {
    /// Retreives user-facing name for the ScaleMode.
    pub fn name(&self) -> &'static str {
        match self {
            ScaleMode::None => "None",
            ScaleMode::Shrink => "Shrink",
            ScaleMode::Full => "Full",
            ScaleMode::Crop => "Crop",
        }
    }
}

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

/// Represents an image brightness adjustment value restricted to [-255, 255].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Brightness(i32);

impl Brightness {
    /// Zero brightness adjustment.
    pub const ZERO: Self = Self(0);

    /// Constructor that clamps the value to [-255, 255].
    pub fn new(val: i32) -> Self {
        Self(val.clamp(-255, 255))
    }

    /// Access the underlying raw i32 value.
    pub fn value(self) -> i32 {
        self.0
    }

    /// Mutably adjust by a delta, clamping internally.
    pub fn adjust(&mut self, delta: i32) {
        self.0 = (self.0.saturating_add(delta)).clamp(-255, 255);
    }
}

/// Represents an image contrast adjustment value restricted to [-255.0, 255.0].
#[derive(Debug, Clone, Copy, Default)]
pub struct Contrast(f32);

impl Contrast {
    /// Zero contrast adjustment.
    pub const ZERO: Self = Self(0.0);

    /// Constructor that clamps the value to [-255.0, 255.0].
    pub fn new(val: f32) -> Self {
        Self(if val.is_nan() { 0.0 } else { val.clamp(-255.0, 255.0) })
    }

    /// Access the underlying raw f32 value.
    pub fn value(self) -> f32 {
        self.0
    }

    /// Mutably adjust by a delta, clamping internally.
    pub fn adjust(&mut self, delta: f32) {
        self.0 = (self.0 + delta).clamp(-255.0, 255.0);
    }

    /// Update with a new value if it differs from the current value by more than f32::EPSILON.
    pub fn update(&mut self, new_val: f32) -> bool {
        let proposed = new_val.clamp(-255.0, 255.0);
        if (proposed - self.0).abs() > f32::EPSILON {
            self.0 = proposed;
            true
        } else {
            false
        }
    }
}

impl PartialEq for Contrast {
    fn eq(&self, other: &Self) -> bool {
        (self.0 - other.0).abs() <= f32::EPSILON
    }
}

/// Stores viewport pan offsets, clamped relative to image dimensions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PanOffset {
    /// X pan offset in pixels.
    pub x: i64,
    /// Y pan offset in pixels.
    pub y: i64,
}

impl PanOffset {
    /// Zero pan offset.
    pub const ZERO: Self = Self { x: 0, y: 0 };

    /// Constructor with starting coordinates.
    #[allow(dead_code)]
    pub fn new(x: i64, y: i64) -> Self {
        Self { x, y }
    }

    /// Clamps the panning offset limits relative to the image size.
    pub fn clamp(&mut self, img_width: u32, img_height: u32) {
        let max_pan_x = (img_width as i64 / 2).max(0);
        let max_pan_y = (img_height as i64 / 2).max(0);
        self.x = self.x.clamp(-max_pan_x, max_pan_x);
        self.y = self.y.clamp(-max_pan_y, max_pan_y);
    }
}

/// A crop viewport in canvas/image space (can extend past image bounds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CropBox {
    /// Left coordinate.
    pub x1: i64,
    /// Top coordinate.
    pub y1: i64,
    /// Right coordinate.
    pub x2: i64,
    /// Bottom coordinate.
    pub y2: i64,
}

impl CropBox {
    /// Constructor that normalizes coordinates so that x1 <= x2 and y1 <= y2.
    pub fn new(x1: i64, y1: i64, x2: i64, y2: i64) -> Self {
        Self {
            x1: x1.min(x2),
            y1: y1.min(y2),
            x2: x1.max(x2),
            y2: y1.max(y2),
        }
    }

    /// Calculates current width of the crop box.
    #[allow(dead_code)]
    pub fn width(&self) -> u64 {
        (self.x2 - self.x1) as u64
    }

    /// Calculates current height of the crop box.
    #[allow(dead_code)]
    pub fn height(&self) -> u64 {
        (self.y2 - self.y1) as u64
    }
}

/// The actual visible intersection region clamped to image dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageIntersection {
    /// Left clamped coordinate.
    pub x1: u32,
    /// Top clamped coordinate.
    pub y1: u32,
    /// Right clamped coordinate.
    pub x2: u32,
    /// Bottom clamped coordinate.
    pub y2: u32,
}

impl ImageIntersection {
    /// Constructor that normalizes coordinates so that x1 <= x2 and y1 <= y2.
    pub fn new(x1: u32, y1: u32, x2: u32, y2: u32) -> Self {
        Self {
            x1: x1.min(x2),
            y1: y1.min(y2),
            x2: x1.max(x2),
            y2: y1.max(y2),
        }
    }

    /// Clamped width of the intersection.
    pub fn width(&self) -> u32 {
        self.x2 - self.x1
    }

    /// Clamped height of the intersection.
    pub fn height(&self) -> u32 {
        self.y2 - self.y1
    }

    /// If true, the intersection is empty (no overlapping region).
    pub fn is_empty(&self) -> bool {
        self.x1 >= self.x2 || self.y1 >= self.y2
    }
}

/// A request payload sent to the resizing worker threads.
pub struct ResizeRequest {
    /// Shared reference to the decoded dynamic image.
    pub img: Arc<DynamicImage>,
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
    /// The decode result containing the dynamic image, width, height, and icon.
    pub result: Result<(DynamicImage, u32, u32, &'static str), String>,
    /// Whether this was a background prefetch request.
    pub is_prefetch: bool,
    /// Navigation sequence identifier of the load request.
    pub sequence: u64,
}

/// Decodes an image from local paths or comic book archives.
/// Employs zune-jpeg for extremely fast decoding of JPEGs.
pub fn decode_image_source(
    source: ImageSource,
) -> Result<(DynamicImage, u32, u32, &'static str), String> {
    match source {
        ImageSource::Local(path) => {
            let format = image::ImageReader::open(&path)
                .and_then(|r| r.with_guessed_format())
                .map(|r| r.format());

            if let Ok(Some(image::ImageFormat::Jpeg)) = format
                && let Ok(bytes) = std::fs::read(&path)
            {
                let options = zune_jpeg::zune_core::options::DecoderOptions::default()
                    .jpeg_set_out_colorspace(zune_jpeg::zune_core::colorspace::ColorSpace::RGBA);
                let mut decoder = zune_jpeg::JpegDecoder::new_with_options(&bytes, options);
                if let Ok(pixels) = decoder.decode()
                    && let Some(info) = decoder.info()
                    && let Some(rgba_img) =
                        image::RgbaImage::from_raw(info.width as u32, info.height as u32, pixels)
                {
                    let orientation = match image::ImageReader::open(&path)
                        .and_then(|r| r.with_guessed_format())
                    {
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
                    return Ok((img, w, h, "\u{F0225}"));
                }
            }

            let reader = image::ImageReader::open(&path)
                .map_err(|e| format!("Failed to open file:\n{}\n\nError: {}", path.display(), e))?
                .with_guessed_format()
                .map_err(|e| {
                    format!(
                        "Failed to guess format for:\n{}\n\nError: {}",
                        path.display(),
                        e
                    )
                })?;

            let fmt = reader.format();
            let icon = match fmt {
                Some(image::ImageFormat::Jpeg) => "\u{F0225}",
                Some(image::ImageFormat::Png) => "\u{F0E2D}",
                Some(image::ImageFormat::Gif) => "\u{F0D78}",
                _ => "\u{F021F}",
            };

            let mut decoder = reader.into_decoder().map_err(|e| {
                format!(
                    "Failed to read metadata:\n{}\n\nError: {}",
                    path.display(),
                    e
                )
            })?;

            let orientation = decoder
                .orientation()
                .unwrap_or(image::metadata::Orientation::NoTransforms);
            let mut img = image::DynamicImage::from_decoder(decoder).map_err(|e| {
                format!(
                    "Failed to decode image:\n{}\n\nError: {}",
                    path.display(),
                    e
                )
            })?;

            img.apply_orientation(orientation);
            let rgba_img = img.to_rgba8();
            let w = rgba_img.width();
            let h = rgba_img.height();
            Ok((image::DynamicImage::ImageRgba8(rgba_img), w, h, icon))
        }
        ImageSource::Cbz {
            zip_path,
            file_in_zip,
        } => {
            let file = std::fs::File::open(&zip_path)
                .map_err(|e| format!("Failed to open zip file {}: {}", zip_path.display(), e))?;
            let reader = std::io::BufReader::new(file);
            let mut archive = zip::ZipArchive::new(reader)
                .map_err(|e| format!("Failed to read zip archive {}: {}", zip_path.display(), e))?;
            let mut zip_entry = archive.by_name(&file_in_zip).map_err(|e| {
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

            let cursor = std::io::Cursor::new(buffer.clone());
            let reader = image::ImageReader::new(cursor)
                .with_guessed_format()
                .map_err(|e| format!("Failed to guess image format for {}: {}", file_in_zip, e))?;

            if let Some(image::ImageFormat::Jpeg) = reader.format() {
                let options = zune_jpeg::zune_core::options::DecoderOptions::default()
                    .jpeg_set_out_colorspace(zune_jpeg::zune_core::colorspace::ColorSpace::RGBA);
                let mut decoder = zune_jpeg::JpegDecoder::new_with_options(&buffer, options);
                if let Ok(pixels) = decoder.decode()
                    && let Some(info) = decoder.info()
                    && let Some(rgba_img) =
                        image::RgbaImage::from_raw(info.width as u32, info.height as u32, pixels)
                {
                    let cursor_meta = std::io::Cursor::new(buffer);
                    let orientation =
                        match image::ImageReader::new(cursor_meta).with_guessed_format() {
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
                    return Ok((img, w, h, "\u{F0225}"));
                }
            }

            let cursor = std::io::Cursor::new(buffer);
            let reader = image::ImageReader::new(cursor)
                .with_guessed_format()
                .map_err(|e| format!("Failed to guess image format for {}: {}", file_in_zip, e))?;

            let fmt = reader.format();
            let icon = match fmt {
                Some(image::ImageFormat::Jpeg) => "\u{F0225}",
                Some(image::ImageFormat::Png) => "\u{F0E2D}",
                Some(image::ImageFormat::Gif) => "\u{F0D78}",
                _ => "\u{F021F}",
            };

            let mut decoder = reader
                .into_decoder()
                .map_err(|e| format!("Failed to decode header for {}: {}", file_in_zip, e))?;
            let orientation = decoder
                .orientation()
                .unwrap_or(image::metadata::Orientation::NoTransforms);
            let mut img = image::DynamicImage::from_decoder(decoder)
                .map_err(|e| format!("Failed to decode image data for {}: {}", file_in_zip, e))?;
            img.apply_orientation(orientation);

            let rgba_img = img.to_rgba8();
            let w = rgba_img.width();
            let h = rgba_img.height();
            Ok((image::DynamicImage::ImageRgba8(rgba_img), w, h, icon))
        }
    }
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
pub fn process_resize(req: ResizeRequest, resizer: &mut fir::Resizer) -> StatefulProtocol {
    let mut canvas = if req.intersection.x1 as i64 == req.crop.x1
        && req.intersection.x2 as i64 == req.crop.x2
        && req.intersection.y1 as i64 == req.crop.y1
        && req.intersection.y2 as i64 == req.crop.y2
    {
        let crop_rect = Some((
            req.intersection.x1 as f64,
            req.intersection.y1 as f64,
            req.intersection.width() as f64,
            req.intersection.height() as f64,
        ));
        match fast_resize(
            resizer,
            &req.img,
            req.target_w,
            req.target_h,
            req.filter_type,
            crop_rect,
        ) {
            Ok(resized) => resized,
            Err(_) => {
                let cropped_part = req.img.crop_imm(
                    req.intersection.x1,
                    req.intersection.y1,
                    req.intersection.width(),
                    req.intersection.height(),
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

        if !req.intersection.is_empty() {
            let target_inter_w =
                ((req.intersection.width() as f64 * req.scale).round() as u32).max(1);
            let target_inter_h =
                ((req.intersection.height() as f64 * req.scale).round() as u32).max(1);

            let crop_rect = Some((
                req.intersection.x1 as f64,
                req.intersection.y1 as f64,
                req.intersection.width() as f64,
                req.intersection.height() as f64,
            ));

            let resized_part = match fast_resize(
                resizer,
                &req.img,
                target_inter_w,
                target_inter_h,
                req.filter_type,
                crop_rect,
            ) {
                Ok(resized) => resized,
                Err(_) => {
                    let cropped_part = req.img.crop_imm(
                        req.intersection.x1,
                        req.intersection.y1,
                        req.intersection.width(),
                        req.intersection.height(),
                    );
                    cropped_part.resize(
                        target_inter_w,
                        target_inter_h,
                        req.filter_type.to_image_filter(),
                    )
                }
            };

            let paste_x = ((req.intersection.x1 as i64 - req.crop.x1) as f64 * req.scale).round() as i64;
            let paste_y = ((req.intersection.y1 as i64 - req.crop.y1) as f64 * req.scale).round() as i64;

            let paste_x =
                paste_x.clamp(0, (req.target_w as i64 - target_inter_w as i64).max(0)) as u32;
            let paste_y =
                paste_y.clamp(0, (req.target_h as i64 - target_inter_h as i64).max(0)) as u32;

            if let Some(rgba_part) = resized_part.as_rgba8() {
                let _ = screen_canvas.copy_from(rgba_part, paste_x, paste_y);
            } else {
                let _ = screen_canvas.copy_from(&resized_part.to_rgba8(), paste_x, paste_y);
            }
        }
        DynamicImage::ImageRgba8(screen_canvas)
    };

    if req.brightness.value() != 0 {
        canvas = canvas.brighten(req.brightness.value());
    }
    if req.contrast.value() != 0.0 {
        canvas = canvas.adjust_contrast(req.contrast.value());
    }

    req.picker.new_resize_protocol(canvas)
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
