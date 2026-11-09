use fast_image_resize as fir;
use image::DynamicImage;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Resizing filter types for scaling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterType {
    /// Nearest neighbor.
    Nearest,
    /// Box / area-averaging filter.
    Box,
    /// Linear interpolation.
    Linear,
    /// Mitchell-Netravali cubic filter.
    Mitchell,
    /// Lanczos3 windowed sinc filter.
    Lanczos,
}

impl FilterType {
    /// Maps our `FilterType` variants to the `image::imageops::FilterType` counterparts.
    pub fn to_image_filter(self) -> image::imageops::FilterType {
        match self {
            FilterType::Nearest => image::imageops::FilterType::Nearest,
            FilterType::Box => image::imageops::FilterType::Triangle,
            FilterType::Linear => image::imageops::FilterType::Triangle,
            FilterType::Mitchell => image::imageops::FilterType::CatmullRom,
            FilterType::Lanczos => image::imageops::FilterType::Lanczos3,
        }
    }
}

/// Analytical single-sine Lanczos3 filter kernel.
///
/// Uses the triple-angle trigonometric identity $\sin(\pi x) = \sin(3\theta) = \sin(\theta)(3 - 4\sin^2\theta)$
/// where $\theta = \frac{\pi}{3} x$, evaluating only a single sine call per tap instead of two while
/// remaining mathematically identical to $\text{sinc}(x) \cdot \text{sinc}(x / 3.0)$.
pub fn fast_lanczos3_kernel(x: f64) -> f64 {
    if x.abs() < 1e-8 {
        return 1.0;
    }
    if !(-3.0..3.0).contains(&x) {
        return 0.0;
    }
    let theta = (std::f64::consts::PI / 3.0) * x;
    let s = theta.sin();
    let s2 = s * s;
    (s2 * (3.0 - 4.0 * s2)) / (3.0 * theta * theta)
}

/// Division-free Horner polynomial Mitchell-Netravali ($B = 1/3, C = 1/3$) filter kernel.
///
/// Evaluates the cubic piecewise polynomial using Horner's method with compile-time reciprocal constants.
pub fn fast_mitchell_kernel(mut x: f64) -> f64 {
    x = x.abs();
    if x < 1.0 {
        ((7.0 / 6.0) * x - 2.0) * x * x + (8.0 / 9.0)
    } else if x < 2.0 {
        (((-7.0 / 18.0) * x + 2.0) * x - (10.0 / 3.0)) * x + (16.0 / 9.0)
    } else {
        0.0
    }
}

/// Maps `FilterType` to `fir::ResizeAlg`, utilizing pure SIMD Convolution kernels
/// with analytical formulations for Mitchell and Lanczos.
pub fn to_fir_resize_alg(filter_type: FilterType) -> fir::ResizeAlg {
    match filter_type {
        FilterType::Nearest => fir::ResizeAlg::Nearest,
        FilterType::Box => fir::ResizeAlg::Convolution(fir::FilterType::Box),
        FilterType::Linear => fir::ResizeAlg::Convolution(fir::FilterType::Bilinear),
        FilterType::Mitchell => fir::ResizeAlg::Convolution(fir::FilterType::Custom(
            fir::Filter::new("Mitchell", fast_mitchell_kernel, 2.0)
                .expect("Valid custom Mitchell filter"),
        )),
        FilterType::Lanczos => fir::ResizeAlg::Convolution(fir::FilterType::Custom(
            fir::Filter::new("Lanczos3", fast_lanczos3_kernel, 3.0)
                .expect("Valid custom Lanczos3 filter"),
        )),
    }
}

/// Maximum source or destination pixel count before `fir::Resizer` scratch memory is reset.
/// Resizing images larger than this threshold (>16 MP) causes `Resizer` to allocate large
/// internal scratch buffers; resetting back to a fresh instance releases this peak memory.
pub const MAX_RESIZER_SCRATCH_PIXELS: u64 = 16_000_000;

/// Resets a `fir::Resizer` instance back to `fir::Resizer::new()` if the processed dimensions
/// exceeded `MAX_RESIZER_SCRATCH_PIXELS`.
pub fn maybe_reclaim_resizer_scratch(
    resizer: &mut fir::Resizer,
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) {
    let src_pixels = (src_w as u64) * (src_h as u64);
    let dst_pixels = (dst_w as u64) * (dst_h as u64);
    if src_pixels > MAX_RESIZER_SCRATCH_PIXELS || dst_pixels > MAX_RESIZER_SCRATCH_PIXELS {
        *resizer = fir::Resizer::new();
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
            ScaleMode::Full => "Fit",
            ScaleMode::Crop => "Crop",
        }
    }
}

/// Source of a fast thumbnail placeholder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbnailSource {
    /// Embedded EXIF/Preview IFD thumbnail extracted from the image header.
    Exif,
    /// Freedesktop.org XDG shared thumbnail cache (`~/.cache/thumbnails`).
    XdgCache,
}

impl ThumbnailSource {
    /// Returns the short human-readable label for the thumbnail source.
    pub const fn display_label(self) -> &'static str {
        match self {
            Self::Exif => "EXIF",
            Self::XdgCache => "XDG",
        }
    }
}

/// Represents an image brightness adjustment value restricted to [-255, 255].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize)]
#[serde(transparent)]
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

    /// Returns true if this brightness adjustment is zero.
    pub fn is_zero(&self) -> bool {
        self.0 == 0
    }

    /// Mutably adjust by a delta, clamping internally.
    pub fn adjust(&mut self, delta: i32) {
        self.0 = (self.0.saturating_add(delta)).clamp(-255, 255);
    }
}

impl<'de> Deserialize<'de> for Brightness {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let val = i32::deserialize(deserializer)?;
        Ok(Self::new(val))
    }
}

impl std::str::FromStr for Brightness {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let val = s.trim().parse::<i32>()?;
        Ok(Self::new(val))
    }
}

/// Represents an image contrast adjustment value restricted to [-255.0, 255.0].
#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(transparent)]
pub struct Contrast(f32);

impl Contrast {
    /// Zero contrast adjustment.
    pub const ZERO: Self = Self(0.0);

    /// Constructor that clamps the value to [-255.0, 255.0].
    pub fn new(val: f32) -> Self {
        Self(if val.is_nan() {
            0.0
        } else {
            val.clamp(-255.0, 255.0)
        })
    }

    /// Access the underlying raw f32 value.
    pub fn value(self) -> f32 {
        self.0
    }

    /// Returns true if this contrast adjustment is zero.
    pub fn is_zero(&self) -> bool {
        self.0 == 0.0
    }

    /// Mutably adjust by a delta, clamping internally.
    pub fn adjust(&mut self, delta: f32) {
        if !delta.is_nan() {
            self.0 = (self.0 + delta).clamp(-255.0, 255.0);
        }
    }

    /// Update with a new value if it differs from the current value by more than f32::EPSILON.
    pub fn update(&mut self, new_val: f32) -> bool {
        if new_val.is_nan() {
            return false;
        }
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

impl<'de> Deserialize<'de> for Contrast {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let val = f32::deserialize(deserializer)?;
        Ok(Self::new(val))
    }
}

impl std::str::FromStr for Contrast {
    type Err = std::num::ParseFloatError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let val = s.trim().parse::<f32>()?;
        Ok(Self::new(val))
    }
}

/// Represents an image rotation in 90-degree increments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Rotation {
    /// No rotation (0 degrees).
    #[default]
    D0,
    /// 90 degrees clockwise rotation.
    D90,
    /// 180 degrees rotation.
    D180,
    /// 270 degrees clockwise rotation.
    D270,
}

impl Rotation {
    /// Constructs a `Rotation` from degrees, rounding/modulo to 90 degree increments.
    pub fn from_degrees(deg: u32) -> Self {
        match deg % 360 {
            90 => Self::D90,
            180 => Self::D180,
            270 => Self::D270,
            _ => Self::D0,
        }
    }

    /// Converts the `Rotation` into its degree numeric representation.
    pub fn to_degrees(self) -> u32 {
        match self {
            Self::D0 => 0,
            Self::D90 => 90,
            Self::D180 => 180,
            Self::D270 => 270,
        }
    }

    /// Returns true if this rotation adjustment is zero degrees.
    pub fn is_zero(&self) -> bool {
        matches!(self, Self::D0)
    }

    /// Returns the rotation resulting from turning clockwise by 90 degrees.
    pub fn rotate_clockwise(self) -> Self {
        match self {
            Self::D0 => Self::D90,
            Self::D90 => Self::D180,
            Self::D180 => Self::D270,
            Self::D270 => Self::D0,
        }
    }

    /// Returns the rotation resulting from turning counter-clockwise by 90 degrees.
    pub fn rotate_counter_clockwise(self) -> Self {
        match self {
            Self::D0 => Self::D270,
            Self::D90 => Self::D0,
            Self::D180 => Self::D90,
            Self::D270 => Self::D180,
        }
    }
}

impl Serialize for Rotation {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_u32(self.to_degrees())
    }
}

impl<'de> Deserialize<'de> for Rotation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let deg = u32::deserialize(deserializer)?;
        Ok(Self::from_degrees(deg))
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
    #[cfg(test)]
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
    pub fn width(&self) -> u64 {
        (self.x2 - self.x1) as u64
    }

    /// Calculates current height of the crop box.
    #[cfg(test)]
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

/// Represents a zoom factor restricted to a safe range [0.0001, 1000.0] (100000%).
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct ZoomFactor(f64);

impl ZoomFactor {
    /// The default zoom factor (100%).
    pub const DEFAULT: Self = Self(1.0);

    /// Constructor that clamps the value to [0.0001, 1000.0].
    pub fn new(val: f64) -> Self {
        Self(if val.is_nan() {
            1.0
        } else {
            val.clamp(0.0001, 1000.0)
        })
    }

    /// Access the underlying raw f64 value.
    pub fn value(self) -> f64 {
        self.0
    }
}

impl Default for ZoomFactor {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Terminal cell grid dimensions (columns and rows).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CellDimensions {
    /// Width in terminal columns/cells.
    pub cols: u16,
    /// Height in terminal rows/cells.
    pub rows: u16,
}

impl CellDimensions {
    /// Creates a new cell dimension struct.
    pub const fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }

    /// Returns true if either columns or rows are zero.
    pub const fn is_empty(&self) -> bool {
        self.cols == 0 || self.rows == 0
    }
}

/// Image pixel dimensions (width and height).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PixelDimensions {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl PixelDimensions {
    /// Creates a new pixel dimension struct.
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// Calculates megapixels as a floating-point count.
    pub fn megapixels(self) -> f64 {
        (self.width as f64 * self.height as f64) / 1_000_000.0
    }

    /// Returns true if either width or height is zero.
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// Returns `true` if applying `orientation` swaps the image width and height dimensions
/// (i.e., 90° or 270° rotations, with or without horizontal flip).
#[inline]
pub const fn orientation_swaps_dimensions(orientation: image::metadata::Orientation) -> bool {
    matches!(
        orientation,
        image::metadata::Orientation::Rotate90
            | image::metadata::Orientation::Rotate270
            | image::metadata::Orientation::Rotate90FlipH
            | image::metadata::Orientation::Rotate270FlipH
    )
}

/// Known file extensions for RAW camera image containers.
pub const RAW_EXTENSIONS: &[&str] = &["dng", "cr2", "nef", "arw", "orf", "rw2", "pef", "raf"];

/// Returns true if the lowercase file extension corresponds to a RAW camera format.
#[inline]
pub fn is_raw_ext(ext: &str) -> bool {
    RAW_EXTENSIONS
        .iter()
        .any(|&raw_ext| ext.eq_ignore_ascii_case(raw_ext))
}

/// The source input format for an image.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// Returns a unique identifier string for the image source, using the provided current working directory.
    pub fn identifier_with_cwd(&self, cwd: Option<&Path>) -> String {
        match self {
            Self::Local(path) => {
                let abs = if path.is_absolute() {
                    path.clone()
                } else if let Some(curr) = cwd {
                    curr.join(path)
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
                } else if let Some(curr) = cwd {
                    curr.join(zip_path)
                } else if let Ok(curr) = std::env::current_dir() {
                    curr.join(zip_path)
                } else {
                    zip_path.clone()
                };
                format!("{}::{}", abs_zip.to_string_lossy(), file_in_zip)
            }
        }
    }

    /// Returns a unique absolute identifier string for the image source (local path or cbz path + page).
    pub fn identifier(&self) -> String {
        self.identifier_with_cwd(std::env::current_dir().ok().as_deref())
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

    /// Returns the file extension of the image source as a string slice, if present.
    pub fn extension(&self) -> Option<&str> {
        let ext = match self {
            Self::Local(path) => path.extension()?,
            Self::Cbz { file_in_zip, .. } => Path::new(file_in_zip).extension()?,
        };
        ext.to_str()
    }

    /// Returns true if the image source is a RAW format.
    pub fn is_raw(&self) -> bool {
        self.extension().is_some_and(is_raw_ext)
    }
}

/// Specifies the decoding strategy and priority for a `LoaderRequest`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoaderMode {
    /// Active foreground load: extract fast thumbnail placeholder first, then decode full resolution.
    ThumbnailAndFull,
    /// Fast-scroll foreground load: extract only the thumbnail placeholder (falling back to full decode only if no thumbnail exists).
    ThumbnailOnly,
    /// Promotion foreground load: thumbnail is already resident; skip thumbnail extraction and decode only full resolution.
    FullOnly,
    /// Background neighbor prefetch load.
    Prefetch,
}

impl LoaderMode {
    /// Returns true if this request is a background neighbor prefetch.
    #[inline]
    pub fn is_prefetch(self) -> bool {
        matches!(self, Self::Prefetch)
    }

    /// Returns true if the loader should attempt EXIF/XDG thumbnail extraction.
    #[inline]
    pub fn should_extract_thumbnail(self, disable_thumbnail: bool) -> bool {
        !disable_thumbnail && matches!(self, Self::ThumbnailAndFull | Self::ThumbnailOnly)
    }

    /// Returns true if the loader should stop after extracting a valid thumbnail.
    #[inline]
    pub fn is_thumbnail_only(self) -> bool {
        matches!(self, Self::ThumbnailOnly)
    }
}

/// A request sent to the persistent loader thread.
pub struct LoaderRequest {
    /// File index inside the App source list.
    pub idx: usize,
    /// The ImageSource identifier.
    pub source: ImageSource,
    /// Decoding strategy and priority mode.
    pub mode: LoaderMode,
    /// Navigation sequence identifier to discard outdated loads.
    pub sequence: u64,
}

/// Encapsulates success values for a decoded image.
pub struct DecodedImage {
    /// The decoded DynamicImage.
    pub image: std::sync::Arc<DynamicImage>,
    /// Width of the image in pixels.
    pub width: u32,
    /// Height of the image in pixels.
    pub height: u32,
    /// Optional format parsed during load.
    pub format: Option<image::ImageFormat>,
    /// The size of the raw source file in bytes.
    pub disk_size: u64,
    /// Sensor dimensions of the raw container (`(width, height)`), if raw format.
    pub raw_dimensions: Option<(u32, u32)>,
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
    /// Source of the fast thumbnail placeholder, if any.
    /// `Some(source)` indicates a fast-load low-resolution thumbnail placeholder from `source`,
    /// while `None` indicates a full-resolution decoded image.
    pub thumbnail_source: Option<ThumbnailSource>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_orientation_swaps_dimensions() {
        use image::metadata::Orientation;

        assert!(!orientation_swaps_dimensions(Orientation::NoTransforms));
        assert!(orientation_swaps_dimensions(Orientation::Rotate90));
        assert!(!orientation_swaps_dimensions(Orientation::Rotate180));
        assert!(orientation_swaps_dimensions(Orientation::Rotate270));
        assert!(!orientation_swaps_dimensions(Orientation::FlipHorizontal));
        assert!(!orientation_swaps_dimensions(Orientation::FlipVertical));
        assert!(orientation_swaps_dimensions(Orientation::Rotate90FlipH));
        assert!(orientation_swaps_dimensions(Orientation::Rotate270FlipH));
    }

    #[test]
    fn test_dimensions_newtypes() {
        let cells = CellDimensions::new(80, 24);
        assert_eq!(cells.cols, 80);
        assert_eq!(cells.rows, 24);
        assert!(!cells.is_empty());

        let empty_cells = CellDimensions::new(0, 24);
        assert!(empty_cells.is_empty());

        let pixels = PixelDimensions::new(1920, 1080);
        assert_eq!(pixels.width, 1920);
        assert_eq!(pixels.height, 1080);
        assert!(!pixels.is_empty());
        assert!((pixels.megapixels() - 2.0736).abs() < 1e-4);

        let empty_pixels = PixelDimensions::new(1920, 0);
        assert!(empty_pixels.is_empty());
    }

    #[test]
    fn test_unpack_rgb24_to_rgba32_simd_and_tail_equivalence() {
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

            let dynamic_rgba = DynamicImage::ImageRgba8(fast_owned);
            match to_rgba8_fast(&dynamic_rgba) {
                std::borrow::Cow::Borrowed(_) => {}
                std::borrow::Cow::Owned(_) => panic!("Expected Cow::Borrowed for ImageRgba8"),
            }
        }
    }
}
