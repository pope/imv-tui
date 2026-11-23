use fast_image_resize as fir;
use image::DynamicImage;
use rayon::prelude::*;
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
    #[cfg(test)]
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

/// Validated non-empty floating-point source crop rectangle strictly clamped within
/// the backing image buffer dimensions `(img_w, img_h)`.
///
/// Guarantees by construction:
/// - `img_dims.width > 0` and `img_dims.height > 0`
/// - `left`, `top`, `width`, `height` are finite
/// - `0.0 <= left < img_dims.width as f64` and `0.0 <= top < img_dims.height as f64`
/// - `width > 0.0` and `height > 0.0`
/// - `left + width <= img_dims.width as f64` and `top + height <= img_dims.height as f64`
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SourceCropRect {
    left: f64,
    top: f64,
    width: f64,
    height: f64,
    img_dims: PixelDimensions,
}

impl SourceCropRect {
    /// Maps an `intersection` in `original_size` pixel space into `img_dims` pixel space.
    ///
    /// Returns `None` if `img_dims` is empty, `intersection` is empty, or `intersection`
    /// lies completely outside the reference bounds. When `img_dims != original_size`
    /// (thumbnail placeholder), coordinates are scaled independently along X and Y via
    /// floating-point `(factor_x, factor_y)` so the sampled region matches normalized-UV
    /// quad mapping without aspect-ratio distortion.
    pub fn map_from_original(
        intersection: ImageIntersection,
        original_size: PixelDimensions,
        img_dims: PixelDimensions,
    ) -> Option<Self> {
        if intersection.is_empty() || img_dims.is_empty() {
            return None;
        }

        let img_w_f = img_dims.width as f64;
        let img_h_f = img_dims.height as f64;

        let (mut left, mut top, mut width, mut height) = if img_dims != original_size
            && !original_size.is_empty()
        {
            if intersection.x1 >= original_size.width || intersection.y1 >= original_size.height {
                return None;
            }
            let factor_x = img_w_f / original_size.width as f64;
            let factor_y = img_h_f / original_size.height as f64;

            let src_left = (intersection.x1 as f64 * factor_x).clamp(0.0, img_w_f);
            let src_top = (intersection.y1 as f64 * factor_y).clamp(0.0, img_h_f);
            let src_w = (intersection.width() as f64 * factor_x).min(img_w_f - src_left);
            let src_h = (intersection.height() as f64 * factor_y).min(img_h_f - src_top);
            (src_left, src_top, src_w, src_h)
        } else {
            if intersection.x1 >= img_dims.width || intersection.y1 >= img_dims.height {
                return None;
            }
            let x2 = intersection.x2.min(img_dims.width);
            let y2 = intersection.y2.min(img_dims.height);
            if intersection.x1 >= x2 || intersection.y1 >= y2 {
                return None;
            }
            (
                intersection.x1 as f64,
                intersection.y1 as f64,
                (x2 - intersection.x1) as f64,
                (y2 - intersection.y1) as f64,
            )
        };

        if !left.is_finite() || !top.is_finite() || !width.is_finite() || !height.is_finite() {
            return None;
        }

        if width <= 0.0 {
            left = left.min((img_dims.width - 1) as f64);
            width = (img_w_f - left).min(1.0);
        } else if left + width > img_w_f {
            width = f64::from_bits(width.to_bits().saturating_sub(1));
        }

        if height <= 0.0 {
            top = top.min((img_dims.height - 1) as f64);
            height = (img_h_f - top).min(1.0);
        } else if top + height > img_h_f {
            height = f64::from_bits(height.to_bits().saturating_sub(1));
        }

        debug_assert!(left >= 0.0 && left < img_w_f);
        debug_assert!(top >= 0.0 && top < img_h_f);
        debug_assert!(width > 0.0 && left + width <= img_w_f);
        debug_assert!(height > 0.0 && top + height <= img_h_f);

        Some(Self {
            left,
            top,
            width,
            height,
            img_dims,
        })
    }

    /// Returns the validated floating-point `(left, top, width, height)` crop tuple
    /// suitable for `fast_image_resize::ResizeOptions::crop`.
    pub const fn as_f64_tuple(self) -> (f64, f64, f64, f64) {
        (self.left, self.top, self.width, self.height)
    }

    /// Converts the validated crop rectangle into safe, non-empty integer `(x, y, width, height)`
    /// bounds strictly within the backing image dimensions for `DynamicImage::crop_imm`.
    pub fn as_u32_bounds(self) -> (u32, u32, u32, u32) {
        let x = (self.left.floor() as u32).min(self.img_dims.width - 1);
        let y = (self.top.floor() as u32).min(self.img_dims.height - 1);
        let max_w = self.img_dims.width - x;
        let max_h = self.img_dims.height - y;
        let w = (self.width.ceil() as u32).clamp(1, max_w);
        let h = (self.height.ceil() as u32).clamp(1, max_h);
        (x, y, w, h)
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

    /// Returns the scheduling and deduplication priority within a same-sequence loader batch
    /// (lower value = higher priority).
    ///
    /// Ensures foreground full-resolution promotions (`ThumbnailAndFull` / `FullOnly`) always
    /// supersede earlier `ThumbnailOnly` or background `Prefetch` requests for the same
    /// `(sequence, idx)` when coalesced in `loader_rx`.
    #[inline]
    pub const fn batch_priority(self) -> u8 {
        match self {
            Self::ThumbnailAndFull => 0,
            Self::FullOnly => 1,
            Self::ThumbnailOnly => 2,
            Self::Prefetch => 3,
        }
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
    /// Image format parsed during load.
    pub format: image::ImageFormat,
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

/// Unpacks packed 24-bit RGB (`[R, G, B]`) bytes into a pre-allocated 32-bit RGBA
/// (`[R, G, B, 255]`) destination slice using 4-pixel (12-byte -> 16-byte) unrolled
/// block unpacking for SIMD auto-vectorization.
///
/// Unpacks `min(rgb.len() / 3, rgba.len() / 4)` complete pixels.
pub fn unpack_rgb24_into_rgba32(rgb: &[u8], rgba: &mut [u8]) {
    let num_pixels = (rgb.len() / 3).min(rgba.len() / 4);
    let (in_chunks, in_rem) = rgb[..num_pixels * 3].as_chunks::<12>();
    let (out_chunks, out_rem) = rgba[..num_pixels * 4].as_chunks_mut::<16>();

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
}

/// Unpacks packed 24-bit RGB (`[R, G, B]`) bytes into 32-bit RGBA (`[R, G, B, 255]`) bytes
/// using 4-pixel (12-byte -> 16-byte) unrolled block unpacking for SIMD auto-vectorization.
pub fn unpack_rgb24_to_rgba32(rgb: &[u8]) -> Vec<u8> {
    let num_pixels = rgb.len() / 3;
    let mut rgba = vec![0u8; num_pixels * 4];
    unpack_rgb24_into_rgba32(rgb, &mut rgba);
    rgba
}

/// Returns `true` if every RGBA8 pixel in `rgba` is fully opaque (`alpha == 0xFF`).
///
/// Processes 16 pixels (64 bytes) per iteration using bitwise AND across all 16 alpha lanes
/// for SIMD auto-vectorization, exiting early on the first non-opaque block, followed by
/// 16-byte (4-pixel) and 4-byte (1-pixel) tail handling.
/// Trailing incomplete pixel bytes (`rgba.len() % 4`) are ignored.
pub fn is_rgba8_opaque(rgba: &[u8]) -> bool {
    let num_pixels = rgba.len() / 4;
    let (chunks64, rem64) = rgba[..num_pixels * 4].as_chunks::<64>();

    for c in chunks64 {
        let a = c[3]
            & c[7]
            & c[11]
            & c[15]
            & c[19]
            & c[23]
            & c[27]
            & c[31]
            & c[35]
            & c[39]
            & c[43]
            & c[47]
            & c[51]
            & c[55]
            & c[59]
            & c[63];
        if a != 0xFF {
            return false;
        }
    }

    let (chunks16, rem16) = rem64.as_chunks::<16>();
    for c in chunks16 {
        if (c[3] & c[7] & c[11] & c[15]) != 0xFF {
            return false;
        }
    }

    for px in rem16.as_chunks::<4>().0 {
        if px[3] != 0xFF {
            return false;
        }
    }

    true
}

/// Minimum pixel count (~1 MP) before `pack_rgba32_to_rgb24` parallelizes block packing
/// and destination page-faulting across `rayon` worker threads.
const PARALLEL_PACK_MIN_PIXELS: usize = 1_000_000;

/// Packs a slice of 4-pixel RGBA blocks (`[u8; 16]`) into 4-pixel RGB blocks (`[u8; 12]`).
#[inline]
fn pack_rgba_blocks(in_blocks: &[[u8; 16]], out_blocks: &mut [[u8; 12]]) {
    for (src, dst) in in_blocks.iter().zip(out_blocks.iter_mut()) {
        *dst = [
            src[0], src[1], src[2], src[4], src[5], src[6], src[8], src[9], src[10], src[12],
            src[13], src[14],
        ];
    }
}

/// Packs 32-bit RGBA (`[R, G, B, A]`) bytes into packed 24-bit RGB (`[R, G, B]`) bytes
/// using 4-pixel (16-byte -> 12-byte) unrolled block packing for SIMD auto-vectorization,
/// parallelized across disjoint destination slabs via `rayon` for large images (`>= 1 MP`).
pub fn pack_rgba32_to_rgb24(rgba: &[u8]) -> Vec<u8> {
    let num_pixels = rgba.len() / 4;
    let mut rgb = vec![0u8; num_pixels * 3];
    let (in_chunks, in_rem) = rgba[..num_pixels * 4].as_chunks::<16>();
    let (out_chunks, out_rem) = rgb.as_chunks_mut::<12>();

    if num_pixels >= PARALLEL_PACK_MIN_PIXELS {
        let target_slabs = rayon::current_num_threads().max(1) * 4;
        let slab_blocks = out_chunks.len().div_ceil(target_slabs).max(1);
        out_chunks
            .par_chunks_mut(slab_blocks)
            .enumerate()
            .for_each(|(slab_idx, dst_slab)| {
                let start = slab_idx * slab_blocks;
                let src_slab = &in_chunks[start..start + dst_slab.len()];
                pack_rgba_blocks(src_slab, dst_slab);
            });
    } else {
        pack_rgba_blocks(in_chunks, out_chunks);
    }

    for (src, dst) in in_rem
        .as_chunks::<4>()
        .0
        .iter()
        .zip(out_rem.as_chunks_mut::<3>().0.iter_mut())
    {
        *dst = [src[0], src[1], src[2]];
    }

    rgb
}

/// Strips redundant fully-opaque alpha channels from `DynamicImage::ImageRgba8` buffers,
/// converting them to `DynamicImage::ImageRgb8` via SIMD-friendly packing.
///
/// Images that contain any pixel with `alpha < 255`, or that are already another variant,
/// are returned untouched.
pub fn strip_opaque_alpha(img: DynamicImage) -> DynamicImage {
    match img {
        DynamicImage::ImageRgba8(rgba) if is_rgba8_opaque(rgba.as_raw()) => {
            let (w, h) = (rgba.width(), rgba.height());
            let raw = pack_rgba32_to_rgb24(rgba.as_raw());
            DynamicImage::ImageRgb8(
                image::RgbImage::from_raw(w, h, raw).expect("valid RGB dimensions"),
            )
        }
        other => other,
    }
}

/// Normalizes a decoded `DynamicImage` into either `DynamicImage::ImageRgb8` (for opaque images)
/// or `DynamicImage::ImageRgba8` (for images with transparency).
///
/// Formats without an alpha channel (e.g. `ImageLuma8`, `ImageRgb16`) are converted directly to
/// `ImageRgb8`; `ImageRgba8` and other alpha-capable formats are checked via `strip_opaque_alpha`
/// so opaque buffers avoid alpha premultiplication during resizing and use 25% less memory.
pub fn normalize_decoded_image(img: DynamicImage) -> DynamicImage {
    match img {
        DynamicImage::ImageRgb8(_) => img,
        DynamicImage::ImageRgba8(_) => strip_opaque_alpha(img),
        DynamicImage::ImageLuma8(_)
        | DynamicImage::ImageLuma16(_)
        | DynamicImage::ImageRgb16(_)
        | DynamicImage::ImageRgb32F(_) => DynamicImage::ImageRgb8(img.into_rgb8()),
        other => strip_opaque_alpha(DynamicImage::ImageRgba8(into_rgba8_fast(other))),
    }
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

/// Cache-blocked tile edge length (in pixels) for fast 90°/270° image rotations.
/// A 32×32 tile of RGB8 (3 KB) or RGBA8 (4 KB) fits comfortably inside L1D cache.
const ROTATE_TILE_SIZE: usize = 32;

/// Minimum pixel count (~1 MP) before rotation dispatches to `rayon` to parallelize
/// page-faulting and memory bandwidth across disjoint destination slabs.
const PARALLEL_ROTATE_MIN_PIXELS: usize = 1_000_000;

/// Computes the tile-aligned destination slab height (in rows) for parallel 90°/270°
/// rotation, targeting ~4 slabs per `rayon` worker thread for balanced work-stealing.
fn rotate_slab_rows(dst_rows: usize) -> usize {
    let target_slabs = rayon::current_num_threads().max(1) * 4;
    let tiles_per_slab = dst_rows
        .div_ceil(ROTATE_TILE_SIZE)
        .div_ceil(target_slabs)
        .max(1);
    tiles_per_slab * ROTATE_TILE_SIZE
}

/// Rotates a contiguous horizontal destination slab (`dst_y_start..dst_y_start + slab_rows`)
/// of a 90° clockwise rotation from `src_px` (`w × h`) into `dst_slab` (`slab_rows × h`),
/// using destination-sequential outer-`tx`, inner-`ty` cache-blocked tiling.
fn rotate90_slab<const N: usize>(
    src_px: &[[u8; N]],
    dst_slab: &mut [[u8; N]],
    w: usize,
    h: usize,
    dst_y_start: usize,
) {
    let slab_rows = dst_slab.len() / h;
    let mut tile = [[0u8; N]; ROTATE_TILE_SIZE * ROTATE_TILE_SIZE];

    for local_row_start in (0..slab_rows).step_by(ROTATE_TILE_SIZE) {
        let bw = (slab_rows - local_row_start).min(ROTATE_TILE_SIZE);
        let tx = dst_y_start + local_row_start;

        for ty in (0..h).step_by(ROTATE_TILE_SIZE) {
            let bh = (h - ty).min(ROTATE_TILE_SIZE);
            let dst_x_start = h - ty - bh;

            // Sequential row reads from src, transposed into L1-resident stack tile.
            for ly in 0..bh {
                let src_row_start = (ty + ly) * w + tx;
                let src_row = &src_px[src_row_start..src_row_start + bw];
                let col = bh - 1 - ly;
                for (lx, &px) in src_row.iter().enumerate() {
                    tile[lx * ROTATE_TILE_SIZE + col] = px;
                }
            }

            // Sequential row writes from stack tile into destination slab.
            for lx in 0..bw {
                let dst_row_start = (local_row_start + lx) * h + dst_x_start;
                dst_slab[dst_row_start..dst_row_start + bh]
                    .copy_from_slice(&tile[lx * ROTATE_TILE_SIZE..lx * ROTATE_TILE_SIZE + bh]);
            }
        }
    }
}

/// Rotates a tightly packed `w × h` pixel buffer (with `N` bytes per pixel) 90° clockwise
/// into a `h × w` buffer using destination-sequential cache-blocked tiling, parallelized
/// across disjoint destination row slabs via `rayon` for large images (`>= 1 MP`).
fn rotate90_pixels<const N: usize>(src: &[u8], w: u32, h: u32) -> Vec<u8> {
    let w = w as usize;
    let h = h as usize;
    let total_pixels = w * h;
    let mut dst = vec![0u8; total_pixels * N];
    if total_pixels == 0 {
        return dst;
    }

    let src_px = &src.as_chunks::<N>().0[..total_pixels];
    let dst_px = &mut dst.as_chunks_mut::<N>().0[..total_pixels];

    if total_pixels < PARALLEL_ROTATE_MIN_PIXELS || w < ROTATE_TILE_SIZE * 2 {
        rotate90_slab(src_px, dst_px, w, h, 0);
    } else {
        let slab_rows = rotate_slab_rows(w);
        dst_px
            .par_chunks_mut(slab_rows * h)
            .enumerate()
            .for_each(|(slab_idx, dst_slab)| {
                let dst_y_start = slab_idx * slab_rows;
                rotate90_slab(src_px, dst_slab, w, h, dst_y_start);
            });
    }

    dst
}

/// Rotates a tightly packed `w × h` pixel buffer (with `N` bytes per pixel) 180°
/// by reversing the pixel slice sequentially, parallelized across disjoint destination
/// chunks via `rayon` for large images (`>= 1 MP`).
fn rotate180_pixels<const N: usize>(src: &[u8], w: u32, h: u32) -> Vec<u8> {
    let n = (w as usize) * (h as usize);
    let mut dst = vec![0u8; n * N];
    if n == 0 {
        return dst;
    }

    let src_px = &src.as_chunks::<N>().0[..n];
    let dst_px = &mut dst.as_chunks_mut::<N>().0[..n];

    if n < PARALLEL_ROTATE_MIN_PIXELS {
        for (d, s) in dst_px.iter_mut().zip(src_px.iter().rev()) {
            *d = *s;
        }
    } else {
        let target_chunks = rayon::current_num_threads().max(1) * 4;
        let chunk_len = n.div_ceil(target_chunks).max(1);
        dst_px
            .par_chunks_mut(chunk_len)
            .enumerate()
            .for_each(|(chunk_idx, dst_chunk)| {
                let start = chunk_idx * chunk_len;
                let end = start + dst_chunk.len();
                let src_slice = &src_px[n - end..n - start];
                for (d, s) in dst_chunk.iter_mut().zip(src_slice.iter().rev()) {
                    *d = *s;
                }
            });
    }

    dst
}

/// Rotates a contiguous horizontal destination slab (`dst_y_start..dst_y_start + slab_rows`)
/// of a 270° clockwise (90° counter-clockwise) rotation from `src_px` (`w × h`) into
/// `dst_slab` (`slab_rows × h`), using destination-sequential outer-`tx`, inner-`ty` tiling.
fn rotate270_slab<const N: usize>(
    src_px: &[[u8; N]],
    dst_slab: &mut [[u8; N]],
    w: usize,
    h: usize,
    dst_y_start: usize,
) {
    let slab_rows = dst_slab.len() / h;
    let mut tile = [[0u8; N]; ROTATE_TILE_SIZE * ROTATE_TILE_SIZE];

    for local_row_start in (0..slab_rows).step_by(ROTATE_TILE_SIZE) {
        let bw = (slab_rows - local_row_start).min(ROTATE_TILE_SIZE);
        let tx = w - (dst_y_start + local_row_start + bw);

        for ty in (0..h).step_by(ROTATE_TILE_SIZE) {
            let bh = (h - ty).min(ROTATE_TILE_SIZE);

            // Sequential row reads from src, transposed into L1-resident stack tile.
            for ly in 0..bh {
                let src_row_start = (ty + ly) * w + tx;
                let src_row = &src_px[src_row_start..src_row_start + bw];
                for (lx, &px) in src_row.iter().enumerate() {
                    tile[(bw - 1 - lx) * ROTATE_TILE_SIZE + ly] = px;
                }
            }

            // Sequential row writes from stack tile into destination slab.
            for row in 0..bw {
                let dst_row_start = (local_row_start + row) * h + ty;
                dst_slab[dst_row_start..dst_row_start + bh]
                    .copy_from_slice(&tile[row * ROTATE_TILE_SIZE..row * ROTATE_TILE_SIZE + bh]);
            }
        }
    }
}

/// Rotates a tightly packed `w × h` pixel buffer (with `N` bytes per pixel) 270° clockwise
/// (90° counter-clockwise) into a `h × w` buffer using destination-sequential cache-blocked
/// tiling, parallelized across disjoint destination row slabs via `rayon` for large images
/// (`>= 1 MP`).
fn rotate270_pixels<const N: usize>(src: &[u8], w: u32, h: u32) -> Vec<u8> {
    let w = w as usize;
    let h = h as usize;
    let total_pixels = w * h;
    let mut dst = vec![0u8; total_pixels * N];
    if total_pixels == 0 {
        return dst;
    }

    let src_px = &src.as_chunks::<N>().0[..total_pixels];
    let dst_px = &mut dst.as_chunks_mut::<N>().0[..total_pixels];

    if total_pixels < PARALLEL_ROTATE_MIN_PIXELS || w < ROTATE_TILE_SIZE * 2 {
        rotate270_slab(src_px, dst_px, w, h, 0);
    } else {
        let slab_rows = rotate_slab_rows(w);
        dst_px
            .par_chunks_mut(slab_rows * h)
            .enumerate()
            .for_each(|(slab_idx, dst_slab)| {
                let dst_y_start = slab_idx * slab_rows;
                rotate270_slab(src_px, dst_slab, w, h, dst_y_start);
            });
    }

    dst
}

/// Rotates a `DynamicImage` 90° clockwise using cache-blocked tiling for `ImageRgb8`
/// and `ImageRgba8`, falling back to `DynamicImage::rotate90()` for other pixel formats.
pub fn rotate90_fast(img: &DynamicImage) -> DynamicImage {
    match img {
        DynamicImage::ImageRgb8(rgb) => {
            let (w, h) = (rgb.width(), rgb.height());
            let raw = rotate90_pixels::<3>(rgb.as_raw(), w, h);
            DynamicImage::ImageRgb8(
                image::RgbImage::from_raw(h, w, raw).expect("valid rotated RGB dimensions"),
            )
        }
        DynamicImage::ImageRgba8(rgba) => {
            let (w, h) = (rgba.width(), rgba.height());
            let raw = rotate90_pixels::<4>(rgba.as_raw(), w, h);
            DynamicImage::ImageRgba8(
                image::RgbaImage::from_raw(h, w, raw).expect("valid rotated RGBA dimensions"),
            )
        }
        other => other.rotate90(),
    }
}

/// Rotates a `DynamicImage` 180° using sequential pixel reversal for `ImageRgb8`
/// and `ImageRgba8`, falling back to `DynamicImage::rotate180()` for other pixel formats.
pub fn rotate180_fast(img: &DynamicImage) -> DynamicImage {
    match img {
        DynamicImage::ImageRgb8(rgb) => {
            let (w, h) = (rgb.width(), rgb.height());
            let raw = rotate180_pixels::<3>(rgb.as_raw(), w, h);
            DynamicImage::ImageRgb8(
                image::RgbImage::from_raw(w, h, raw).expect("valid rotated RGB dimensions"),
            )
        }
        DynamicImage::ImageRgba8(rgba) => {
            let (w, h) = (rgba.width(), rgba.height());
            let raw = rotate180_pixels::<4>(rgba.as_raw(), w, h);
            DynamicImage::ImageRgba8(
                image::RgbaImage::from_raw(w, h, raw).expect("valid rotated RGBA dimensions"),
            )
        }
        other => other.rotate180(),
    }
}

/// Rotates a `DynamicImage` 270° clockwise (90° counter-clockwise) using cache-blocked tiling
/// for `ImageRgb8` and `ImageRgba8`, falling back to `DynamicImage::rotate270()` for other formats.
pub fn rotate270_fast(img: &DynamicImage) -> DynamicImage {
    match img {
        DynamicImage::ImageRgb8(rgb) => {
            let (w, h) = (rgb.width(), rgb.height());
            let raw = rotate270_pixels::<3>(rgb.as_raw(), w, h);
            DynamicImage::ImageRgb8(
                image::RgbImage::from_raw(h, w, raw).expect("valid rotated RGB dimensions"),
            )
        }
        DynamicImage::ImageRgba8(rgba) => {
            let (w, h) = (rgba.width(), rgba.height());
            let raw = rotate270_pixels::<4>(rgba.as_raw(), w, h);
            DynamicImage::ImageRgba8(
                image::RgbaImage::from_raw(h, w, raw).expect("valid rotated RGBA dimensions"),
            )
        }
        other => other.rotate270(),
    }
}

/// Applies an EXIF `Orientation` transform to a `DynamicImage`, using cache-blocked
/// rotation helpers for 90°/270° transforms and zero-allocation in-place pixel reversal
/// for 180° transforms on `ImageRgb8` and `ImageRgba8`.
pub fn apply_orientation_fast(
    mut img: DynamicImage,
    orientation: image::metadata::Orientation,
) -> DynamicImage {
    use image::metadata::Orientation;
    match orientation {
        Orientation::NoTransforms => img,
        Orientation::Rotate90 => rotate90_fast(&img),
        Orientation::Rotate180 => {
            match img {
                DynamicImage::ImageRgb8(ref mut rgb) => {
                    let n = (rgb.width() as usize) * (rgb.height() as usize);
                    rgb.as_mut().as_chunks_mut::<3>().0[..n].reverse();
                }
                DynamicImage::ImageRgba8(ref mut rgba) => {
                    let n = (rgba.width() as usize) * (rgba.height() as usize);
                    rgba.as_mut().as_chunks_mut::<4>().0[..n].reverse();
                }
                _ => img.apply_orientation(Orientation::Rotate180),
            }
            img
        }
        Orientation::Rotate270 => rotate270_fast(&img),
        Orientation::Rotate90FlipH => {
            let mut rotated = rotate90_fast(&img);
            rotated.apply_orientation(Orientation::FlipHorizontal);
            rotated
        }
        Orientation::Rotate270FlipH => {
            let mut rotated = rotate270_fast(&img);
            rotated.apply_orientation(Orientation::FlipHorizontal);
            rotated
        }
        Orientation::FlipHorizontal | Orientation::FlipVertical => {
            img.apply_orientation(orientation);
            img
        }
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

    #[test]
    fn test_is_rgba8_opaque_edge_cases_and_positions() {
        // Empty slice is vacuously opaque
        assert!(is_rgba8_opaque(&[]));

        // Exhaustively test 1..=33 plus 64-byte/16-pixel chunk boundaries and tails
        // (63, 64, 65, 68, 128, 129 pixels): all opaque, and single non-opaque pixel at every
        // position (covering all 16 lanes of 64-byte chunks, 4 lanes of 16-byte tail, scalar tail,
        // and every position 0..=67).
        for count in (1..=33).chain([63, 64, 65, 68, 128, 129]) {
            let mut rgba = Vec::with_capacity(count * 4);
            for i in 0..count {
                rgba.push(((i * 13) % 256) as u8);
                rgba.push(((i * 29) % 256) as u8);
                rgba.push(((i * 47) % 256) as u8);
                rgba.push(255);
            }
            assert!(
                is_rgba8_opaque(&rgba),
                "All-opaque buffer of {count} pixels must report opaque"
            );

            for pos in 0..count {
                rgba[pos * 4 + 3] = 254;
                assert!(
                    !is_rgba8_opaque(&rgba),
                    "Buffer of {count} pixels with alpha=254 at pixel {pos} must report non-opaque"
                );
                rgba[pos * 4 + 3] = 0;
                assert!(
                    !is_rgba8_opaque(&rgba),
                    "Buffer of {count} pixels with alpha=0 at pixel {pos} must report non-opaque"
                );
                rgba[pos * 4 + 3] = 255;
            }
        }
    }

    #[test]
    fn test_pack_rgba32_to_rgb24_round_trip_and_into_rgb8_equivalence() {
        let pixel_counts = [0, 1, 2, 3, 4, 5, 7, 16, 19, 33];

        for &count in &pixel_counts {
            let mut rgb_orig = Vec::with_capacity(count * 3);
            let mut rgba_varied_alpha = Vec::with_capacity(count * 4);
            for i in 0..count {
                let r = ((i * 7 + 13) % 256) as u8;
                let g = ((i * 11 + 47) % 256) as u8;
                let b = ((i * 13 + 89) % 256) as u8;
                let a = ((i * 17 + 3) % 256) as u8;
                rgb_orig.extend_from_slice(&[r, g, b]);
                rgba_varied_alpha.extend_from_slice(&[r, g, b, a]);
            }

            // Round-trip: rgb -> unpack -> pack -> rgb
            let unpacked = unpack_rgb24_to_rgba32(&rgb_orig);
            let repacked = pack_rgba32_to_rgb24(&unpacked);
            assert_eq!(repacked, rgb_orig);

            // Packing RGBA with arbitrary alpha drops alpha and matches DynamicImage::into_rgb8()
            let packed = pack_rgba32_to_rgb24(&rgba_varied_alpha);
            assert_eq!(packed, rgb_orig);

            if count > 0 {
                let rgba_img =
                    image::RgbaImage::from_raw(count as u32, 1, rgba_varied_alpha).unwrap();
                let std_rgb = DynamicImage::ImageRgba8(rgba_img).into_rgb8();
                assert_eq!(packed, std_rgb.into_raw());
            }
        }
    }

    #[test]
    fn test_pack_rgba32_to_rgb24_parallel_large_buffer() {
        // Exceeds PARALLEL_PACK_MIN_PIXELS (1_000_000) with a non-multiple of 4 and
        // non-multiple of slab size so both parallel slabs and the scalar tail execute.
        let count = PARALLEL_PACK_MIN_PIXELS + 3;
        let mut rgba = Vec::with_capacity(count * 4);
        let mut expected = Vec::with_capacity(count * 3);
        for i in 0..count {
            let r = ((i.wrapping_mul(7) + 13) & 0xFF) as u8;
            let g = ((i.wrapping_mul(11) + 47) & 0xFF) as u8;
            let b = ((i.wrapping_mul(13) + 89) & 0xFF) as u8;
            let a = ((i.wrapping_mul(17) + 3) & 0xFF) as u8;
            rgba.extend_from_slice(&[r, g, b, a]);
            expected.extend_from_slice(&[r, g, b]);
        }

        let packed = pack_rgba32_to_rgb24(&rgba);
        assert_eq!(packed.len(), count * 3);
        assert_eq!(packed, expected);
    }

    #[test]
    fn test_strip_opaque_alpha_and_normalize_decoded_image() {
        // 1. Opaque ImageRgba8 -> ImageRgb8 with identical RGB pixels
        let opaque_rgba = image::RgbaImage::from_fn(7, 5, |x, y| {
            image::Rgba([(x * 31) as u8, (y * 47) as u8, ((x + y) * 19) as u8, 255])
        });
        let expected_rgb = DynamicImage::ImageRgba8(opaque_rgba.clone()).into_rgb8();
        let stripped = strip_opaque_alpha(DynamicImage::ImageRgba8(opaque_rgba));
        match stripped {
            DynamicImage::ImageRgb8(rgb) => assert_eq!(rgb.as_raw(), expected_rgb.as_raw()),
            other => panic!(
                "Expected ImageRgb8 for opaque RGBA8, got {:?}",
                other.color()
            ),
        }

        // 2. Semi-transparent ImageRgba8 (single pixel alpha < 255) -> preserved as ImageRgba8
        let mut trans_rgba = image::RgbaImage::from_fn(7, 5, |x, y| {
            image::Rgba([(x * 31) as u8, (y * 47) as u8, 128, 255])
        });
        trans_rgba.put_pixel(6, 4, image::Rgba([10, 20, 30, 254]));
        let expected_rgba_raw = trans_rgba.as_raw().clone();
        let preserved = strip_opaque_alpha(DynamicImage::ImageRgba8(trans_rgba));
        match preserved {
            DynamicImage::ImageRgba8(rgba) => assert_eq!(rgba.as_raw(), &expected_rgba_raw),
            other => panic!(
                "Expected ImageRgba8 to be preserved when alpha < 255, got {:?}",
                other.color()
            ),
        }

        // 3. Existing ImageRgb8 passes through untouched
        let rgb_img = image::RgbImage::from_fn(5, 3, |x, y| {
            image::Rgb([(x * 40) as u8, (y * 60) as u8, 99])
        });
        let rgb_raw = rgb_img.as_raw().clone();
        match strip_opaque_alpha(DynamicImage::ImageRgb8(rgb_img)) {
            DynamicImage::ImageRgb8(rgb) => assert_eq!(rgb.as_raw(), &rgb_raw),
            other => panic!("Expected ImageRgb8 unchanged, got {:?}", other.color()),
        }

        // 4. normalize_decoded_image converts Luma8 to ImageRgb8 and preserves transparent LumaA8 as ImageRgba8
        let luma = image::GrayImage::from_fn(4, 4, |x, y| image::Luma([(x * 50 + y * 10) as u8]));
        let norm_luma = normalize_decoded_image(DynamicImage::ImageLuma8(luma));
        assert!(matches!(norm_luma, DynamicImage::ImageRgb8(_)));

        let mut luma_a = image::GrayAlphaImage::from_fn(4, 4, |x, _| image::LumaA([x as u8, 255]));
        let norm_opaque_la = normalize_decoded_image(DynamicImage::ImageLumaA8(luma_a.clone()));
        assert!(matches!(norm_opaque_la, DynamicImage::ImageRgb8(_)));

        luma_a.put_pixel(3, 3, image::LumaA([42, 128]));
        let norm_trans_la = normalize_decoded_image(DynamicImage::ImageLumaA8(luma_a));
        assert!(matches!(norm_trans_la, DynamicImage::ImageRgba8(_)));
    }

    #[test]
    fn test_fast_rotations_bit_exact_equivalence() {
        let dims = [
            (0, 0),
            (1, 1),
            (1, 17),
            (17, 1),
            (31, 33),
            (32, 32),
            (64, 64),
            (65, 129),
            (137, 91),
            // Exceeds PARALLEL_ROTATE_MIN_PIXELS with non-tile-multiple dimensions to exercise
            // the multi-threaded slab partitioning path.
            (1027, 1003),
        ];

        for &(w, h) in &dims {
            // ImageRgb8
            let rgb = image::RgbImage::from_fn(w, h, |x, y| {
                image::Rgb([
                    ((x * 13 + y * 7) % 256) as u8,
                    ((x * 29 + y * 11 + 3) % 256) as u8,
                    ((x * 43 + y * 19 + 17) % 256) as u8,
                ])
            });
            let dyn_rgb = DynamicImage::ImageRgb8(rgb);

            let r90_fast = rotate90_fast(&dyn_rgb);
            let r90_ref = dyn_rgb.rotate90();
            assert_eq!(
                (r90_fast.width(), r90_fast.height()),
                (r90_ref.width(), r90_ref.height()),
                "RGB8 rotate90 dims mismatch for {w}x{h}"
            );
            assert_eq!(
                r90_fast.as_bytes(),
                r90_ref.as_bytes(),
                "RGB8 rotate90 pixel mismatch for {w}x{h}"
            );

            let r180_fast = rotate180_fast(&dyn_rgb);
            let r180_ref = dyn_rgb.rotate180();
            assert_eq!(
                (r180_fast.width(), r180_fast.height()),
                (r180_ref.width(), r180_ref.height()),
                "RGB8 rotate180 dims mismatch for {w}x{h}"
            );
            assert_eq!(
                r180_fast.as_bytes(),
                r180_ref.as_bytes(),
                "RGB8 rotate180 pixel mismatch for {w}x{h}"
            );

            let r270_fast = rotate270_fast(&dyn_rgb);
            let r270_ref = dyn_rgb.rotate270();
            assert_eq!(
                (r270_fast.width(), r270_fast.height()),
                (r270_ref.width(), r270_ref.height()),
                "RGB8 rotate270 dims mismatch for {w}x{h}"
            );
            assert_eq!(
                r270_fast.as_bytes(),
                r270_ref.as_bytes(),
                "RGB8 rotate270 pixel mismatch for {w}x{h}"
            );

            // ImageRgba8
            let rgba = image::RgbaImage::from_fn(w, h, |x, y| {
                image::Rgba([
                    ((x * 17 + y * 5) % 256) as u8,
                    ((x * 31 + y * 13 + 7) % 256) as u8,
                    ((x * 47 + y * 23 + 11) % 256) as u8,
                    ((x * 59 + y * 29 + 19) % 256) as u8,
                ])
            });
            let dyn_rgba = DynamicImage::ImageRgba8(rgba);

            let r90_fast = rotate90_fast(&dyn_rgba);
            let r90_ref = dyn_rgba.rotate90();
            assert_eq!(
                (r90_fast.width(), r90_fast.height()),
                (r90_ref.width(), r90_ref.height()),
                "RGBA8 rotate90 dims mismatch for {w}x{h}"
            );
            assert_eq!(
                r90_fast.as_bytes(),
                r90_ref.as_bytes(),
                "RGBA8 rotate90 pixel mismatch for {w}x{h}"
            );

            let r180_fast = rotate180_fast(&dyn_rgba);
            let r180_ref = dyn_rgba.rotate180();
            assert_eq!(
                (r180_fast.width(), r180_fast.height()),
                (r180_ref.width(), r180_ref.height()),
                "RGBA8 rotate180 dims mismatch for {w}x{h}"
            );
            assert_eq!(
                r180_fast.as_bytes(),
                r180_ref.as_bytes(),
                "RGBA8 rotate180 pixel mismatch for {w}x{h}"
            );

            let r270_fast = rotate270_fast(&dyn_rgba);
            let r270_ref = dyn_rgba.rotate270();
            assert_eq!(
                (r270_fast.width(), r270_fast.height()),
                (r270_ref.width(), r270_ref.height()),
                "RGBA8 rotate270 dims mismatch for {w}x{h}"
            );
            assert_eq!(
                r270_fast.as_bytes(),
                r270_ref.as_bytes(),
                "RGBA8 rotate270 pixel mismatch for {w}x{h}"
            );

            // Fallback variant: ImageLuma8
            let luma = image::GrayImage::from_fn(w, h, |x, y| {
                image::Luma([((x * 37 + y * 13 + 5) % 256) as u8])
            });
            let dyn_luma = DynamicImage::ImageLuma8(luma);
            assert_eq!(
                rotate90_fast(&dyn_luma).as_bytes(),
                dyn_luma.rotate90().as_bytes()
            );
            assert_eq!(
                rotate180_fast(&dyn_luma).as_bytes(),
                dyn_luma.rotate180().as_bytes()
            );
            assert_eq!(
                rotate270_fast(&dyn_luma).as_bytes(),
                dyn_luma.rotate270().as_bytes()
            );
        }
    }

    #[test]
    fn test_apply_orientation_fast_all_variants() {
        use image::metadata::Orientation;

        let orientations = [
            Orientation::NoTransforms,
            Orientation::Rotate90,
            Orientation::Rotate180,
            Orientation::Rotate270,
            Orientation::FlipHorizontal,
            Orientation::FlipVertical,
            Orientation::Rotate90FlipH,
            Orientation::Rotate270FlipH,
        ];

        let dims = [
            (0, 0),
            (1, 1),
            (1, 17),
            (17, 1),
            (31, 33),
            (32, 32),
            (64, 64),
            (65, 129),
            (137, 91),
        ];

        for &(w, h) in &dims {
            let rgb = DynamicImage::ImageRgb8(image::RgbImage::from_fn(w, h, |x, y| {
                image::Rgb([
                    ((x * 13 + y * 7) % 256) as u8,
                    ((x * 29 + y * 11 + 3) % 256) as u8,
                    ((x * 43 + y * 19 + 17) % 256) as u8,
                ])
            }));
            let rgba = DynamicImage::ImageRgba8(image::RgbaImage::from_fn(w, h, |x, y| {
                image::Rgba([
                    ((x * 17 + y * 5) % 256) as u8,
                    ((x * 31 + y * 13 + 7) % 256) as u8,
                    ((x * 47 + y * 23 + 11) % 256) as u8,
                    ((x * 59 + y * 29 + 19) % 256) as u8,
                ])
            }));
            let luma = DynamicImage::ImageLuma8(image::GrayImage::from_fn(w, h, |x, y| {
                image::Luma([((x * 37 + y * 13 + 5) % 256) as u8])
            }));

            for &orient in &orientations {
                for (label, base) in [("RGB8", &rgb), ("RGBA8", &rgba), ("Luma8", &luma)] {
                    let fast = apply_orientation_fast(base.clone(), orient);
                    let mut reference = base.clone();
                    reference.apply_orientation(orient);

                    assert_eq!(
                        (fast.width(), fast.height()),
                        (reference.width(), reference.height()),
                        "{label} {orient:?} dims mismatch for {w}x{h}"
                    );
                    assert_eq!(
                        fast.as_bytes(),
                        reference.as_bytes(),
                        "{label} {orient:?} pixel mismatch for {w}x{h}"
                    );
                }
            }
        }
    }

    #[test]
    fn test_source_crop_rect_invariants() {
        let orig = PixelDimensions::new(4416, 2944);
        let thumb = PixelDimensions::new(160, 104);

        // Empty intersection or empty image returns None.
        assert_eq!(
            SourceCropRect::map_from_original(ImageIntersection::new(10, 10, 10, 20), orig, thumb),
            None
        );
        assert_eq!(
            SourceCropRect::map_from_original(
                ImageIntersection::new(0, 0, 100, 100),
                orig,
                PixelDimensions::new(0, 104)
            ),
            None
        );
        // Out-of-bounds intersection returns None.
        assert_eq!(
            SourceCropRect::map_from_original(
                ImageIntersection::new(4416, 0, 4500, 100),
                orig,
                thumb
            ),
            None
        );
        assert_eq!(
            SourceCropRect::map_from_original(
                ImageIntersection::new(160, 0, 200, 50),
                thumb,
                thumb
            ),
            None
        );

        // Full-image intersection maps cleanly to the entire thumbnail.
        let full_thumb = SourceCropRect::map_from_original(
            ImageIntersection::new(0, 0, 4416, 2944),
            orig,
            thumb,
        )
        .expect("full thumbnail crop should be valid");
        let (l, t, w, h) = full_thumb.as_f64_tuple();
        assert_eq!(l, 0.0);
        assert_eq!(t, 0.0);
        assert!((w - 160.0).abs() < 1e-9 && l + w <= 160.0);
        assert!((h - 104.0).abs() < 1e-9 && t + h <= 104.0);
        assert_eq!(full_thumb.as_u32_bounds(), (0, 0, 160, 104));

        // Full-res identity mapping preserves exact integer coordinates.
        let full_res = SourceCropRect::map_from_original(
            ImageIntersection::new(100, 200, 1100, 800),
            orig,
            orig,
        )
        .expect("full-res crop should be valid");
        assert_eq!(full_res.as_f64_tuple(), (100.0, 200.0, 1000.0, 600.0));
        assert_eq!(full_res.as_u32_bounds(), (100, 200, 1000, 600));

        // Extreme 1x1 pixel zoom at the bottom-right corner stays strictly in-bounds.
        let corner_zoom = SourceCropRect::map_from_original(
            ImageIntersection::new(4415, 2943, 4416, 2944),
            orig,
            thumb,
        )
        .expect("corner 1px zoom should produce valid SourceCropRect");
        let (cl, ct, cw, ch) = corner_zoom.as_f64_tuple();
        assert!((0.0..160.0).contains(&cl));
        assert!((0.0..104.0).contains(&ct));
        assert!(cw > 0.0 && cl + cw <= 160.0);
        assert!(ch > 0.0 && ct + ch <= 104.0);
        let (ux, uy, uw, uh) = corner_zoom.as_u32_bounds();
        assert_eq!((ux, uy, uw, uh), (159, 103, 1, 1));
    }
}
