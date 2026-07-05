use serde::{Deserialize, Serialize};

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

/// Represents an image brightness adjustment value restricted to [-255, 255].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
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

impl std::str::FromStr for Brightness {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let val = s.trim().parse::<i32>()?;
        Ok(Self::new(val))
    }
}

/// Represents an image contrast adjustment value restricted to [-255.0, 255.0].
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
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
