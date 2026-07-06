use image::DynamicImage;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A cached image and its associated metadata in the prefetch cache.
#[derive(Clone)]
pub struct CachedImage {
    pub image: Option<Arc<DynamicImage>>,
    pub thumbnail: Option<Arc<DynamicImage>>,
    pub width: u32,
    pub height: u32,
    pub format: Option<image::ImageFormat>,
    pub decode_duration: std::time::Duration,
    pub thumbnail_decode_duration: std::time::Duration,
    pub disk_size: u64,
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
