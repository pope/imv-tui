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

    #[test]
    fn test_brightness_clamping() {
        use crate::imaging::types::Brightness;
        let mut b = Brightness::new(250);
        b.adjust(20);
        assert_eq!(b.value(), 255); // clamped

        let mut b2 = Brightness::new(-250);
        b2.adjust(-30);
        assert_eq!(b2.value(), -255); // clamped
    }

    #[test]
    fn test_rotation_degree_math() {
        use crate::imaging::types::Rotation;
        assert_eq!(Rotation::from_degrees(450), Rotation::D90);
        assert_eq!(Rotation::from_degrees(540), Rotation::D180);
        assert_eq!(Rotation::from_degrees(110), Rotation::D0); // non-multiple of 90 defaults to D0
    }

    #[test]
    fn test_pan_offset_limits() {
        use crate::imaging::types::PanOffset;
        let mut pan = PanOffset::new(1000, -1000);
        // img dimensions 100x200 limits pan to [-50, 50] x [-100, 100]
        pan.clamp(100, 200);
        assert_eq!(pan.x, 50);
        assert_eq!(pan.y, -100);
    }

    #[test]
    fn test_crop_box_normalization() {
        use crate::imaging::types::CropBox;
        let crop = CropBox::new(100, 200, 50, 100);
        assert_eq!(crop.x1, 50);
        assert_eq!(crop.x2, 100);
        assert_eq!(crop.y1, 100);
        assert_eq!(crop.y2, 200);
    }

    #[test]
    fn test_zoom_factor_clamping() {
        use crate::imaging::types::ZoomFactor;
        let z = ZoomFactor::new(2000.0);
        assert_eq!(z.value(), 1000.0); // clamped max

        let z2 = ZoomFactor::new(-5.0);
        assert_eq!(z2.value(), 0.0001); // clamped min

        let z_nan = ZoomFactor::new(f64::NAN);
        assert_eq!(z_nan.value(), 1.0); // NaN reset
    }
}
