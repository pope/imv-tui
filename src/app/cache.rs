use image::DynamicImage;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A cached image and its associated metadata in the prefetch cache.
#[derive(Clone)]
pub struct CachedImage {
    /// The cached full-resolution decoded image, if loaded.
    pub image: Option<Arc<DynamicImage>>,
    /// The cached low-resolution thumbnail image, if loaded.
    pub thumbnail: Option<Arc<DynamicImage>>,
    /// The width of the image.
    pub width: u32,
    /// The height of the image.
    pub height: u32,
    /// The image format (e.g., JPEG, PNG, etc.).
    pub format: Option<image::ImageFormat>,
    /// Time taken to decode the full-resolution image.
    pub decode_duration: std::time::Duration,
    /// Time taken to decode the thumbnail image.
    pub thumbnail_decode_duration: std::time::Duration,
    /// Size of the raw source file on disk in bytes.
    pub disk_size: u64,
    /// The original raw/sensor width of the raw image, if any.
    pub raw_width: Option<u32>,
    /// The original raw/sensor height of the raw image, if any.
    pub raw_height: Option<u32>,
}

pub type PrefetchCache = Arc<Mutex<HashMap<usize, CachedImage>>>;

pub fn get_sliding_window_indices(
    current_index: usize,
    total_images: usize,
    is_visible: impl Fn(usize) -> bool,
) -> Vec<usize> {
    let n = 2; // Cache size N=2 (caches current + 2 before + 2 after)
    let visible: Vec<usize> = (0..total_images).filter(|&idx| is_visible(idx)).collect();
    let total = visible.len();
    if total == 0 {
        return Vec::new();
    }
    let current_pos = visible.iter().position(|&idx| idx == current_index);
    let current_pos = match current_pos {
        Some(pos) => pos,
        None => return Vec::new(),
    };

    let mut indices = Vec::new();
    indices.push(visible[current_pos]);
    for i in 1..=n {
        let prev = if current_pos >= i % total {
            current_pos - (i % total)
        } else {
            current_pos + total - (i % total)
        };
        let next = (current_pos + i) % total;
        indices.push(visible[prev]);
        indices.push(visible[next]);
    }
    indices.sort();
    indices.dedup();
    indices
}

#[cfg(test)]
mod cache_sliding_window_tests {
    use super::*;

    #[test]
    fn test_sliding_window_wrapping_and_filtering() {
        // total 5, current 0, all visible
        let win = get_sliding_window_indices(0, 5, |_| true);
        // Expect: current (0) + prev (4, 3) + next (1, 2) => [0, 1, 2, 3, 4]
        assert_eq!(win, vec![0, 1, 2, 3, 4]);

        // total 5, current 2, index 1 & 3 hidden
        let is_visible = |idx| idx != 1 && idx != 3; // visible: [0, 2, 4]
        let win_filtered = get_sliding_window_indices(2, 5, is_visible);
        // pos of 2 in [0, 2, 4] is 1. N=2 => prev (0, 4) + next (4, 0)
        // Expect: [0, 2, 4]
        assert_eq!(win_filtered, vec![0, 2, 4]);

        // total 1, current 0, visible
        let win_single = get_sliding_window_indices(0, 1, |_| true);
        assert_eq!(win_single, vec![0]);
    }
}
