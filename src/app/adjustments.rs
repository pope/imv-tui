use image::DynamicImage;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// Represents an absolute or relative adjustment to a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adjustment<T> {
    /// An absolute value assignment.
    Absolute(T),
    /// A relative addition to the current value.
    RelativeAdd(T),
    /// A relative subtraction from the current value.
    RelativeSub(T),
}

impl<T: FromStr> FromStr for Adjustment<T> {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err("Empty input".to_string());
        }
        if let Some(stripped) = s.strip_prefix('+') {
            let val = stripped
                .parse::<T>()
                .map_err(|_| "Invalid positive offset".to_string())?;
            Ok(Self::RelativeAdd(val))
        } else if let Some(stripped) = s.strip_prefix('-') {
            let val = stripped
                .parse::<T>()
                .map_err(|_| "Invalid negative offset".to_string())?;
            Ok(Self::RelativeSub(val))
        } else {
            let val = s
                .parse::<T>()
                .map_err(|_| "Invalid absolute value".to_string())?;
            Ok(Self::Absolute(val))
        }
    }
}

/// Individual image adjustments (brightness, contrast, rotation) that are preserved per-file.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ImageAdjustments {
    pub brightness: i32,
    pub contrast: f32,
    pub rotation: u32,
}

impl Default for ImageAdjustments {
    fn default() -> Self {
        Self {
            brightness: 0,
            contrast: 0.0,
            rotation: 0,
        }
    }
}

impl ImageAdjustments {
    /// Applies the rotation setting to the given DynamicImage. Returns None if no rotation is needed.
    pub fn rotate_image(&self, img: &DynamicImage) -> Option<DynamicImage> {
        match self.rotation {
            90 => Some(img.rotate90()),
            180 => Some(img.rotate180()),
            270 => Some(img.rotate270()),
            _ => None,
        }
    }
}
