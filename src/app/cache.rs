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
        let prev = (current_pos + total - i) % total;
        let next = (current_pos + i) % total;
        indices.push(visible[prev]);
        indices.push(visible[next]);
    }
    indices.sort();
    indices.dedup();
    indices
}
