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
        if let Some(stripped) = s.strip_prefix('=') {
            let val = stripped
                .parse::<T>()
                .map_err(|_| "Invalid absolute value".to_string())?;
            Ok(Self::Absolute(val))
        } else if let Some(stripped) = s.strip_prefix('+') {
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

use crate::imaging::types::{Brightness, Contrast, Rotation};

/// Individual image adjustments (brightness, contrast, rotation) that are preserved per-file.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct ImageAdjustments {
    pub brightness: Brightness,
    pub contrast: Contrast,
    pub rotation: Rotation,
}

impl ImageAdjustments {
    /// Applies the rotation setting to the given DynamicImage. Returns None if no rotation is needed.
    pub fn rotate_image(&self, img: &DynamicImage) -> Option<DynamicImage> {
        match self.rotation {
            Rotation::D90 => Some(img.rotate90()),
            Rotation::D180 => Some(img.rotate180()),
            Rotation::D270 => Some(img.rotate270()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imaging::types::Contrast;

    #[test]
    fn test_nan_guards() {
        let mut contrast = Contrast::new(10.0);

        // Adjust with NaN should be ignored and not panic
        contrast.adjust(f32::NAN);
        assert_eq!(contrast.value(), 10.0);

        // Update with NaN should return false and not change value
        assert!(!contrast.update(f32::NAN));
        assert_eq!(contrast.value(), 10.0);
    }

    #[test]
    fn test_absolute_adjustment_prefix() {
        // Absolute with prefix =
        let adj = "=-10".parse::<Adjustment<i32>>().unwrap();
        assert_eq!(adj, Adjustment::Absolute(-10));

        let adj2 = "=50".parse::<Adjustment<i32>>().unwrap();
        assert_eq!(adj2, Adjustment::Absolute(50));

        // Relative with signs
        let adj3 = "-10".parse::<Adjustment<i32>>().unwrap();
        assert_eq!(adj3, Adjustment::RelativeSub(10));

        let adj4 = "+10".parse::<Adjustment<i32>>().unwrap();
        assert_eq!(adj4, Adjustment::RelativeAdd(10));
    }
}
