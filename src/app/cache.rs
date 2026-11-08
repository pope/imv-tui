use crate::app::classifications::ViewMode;
use crate::app::queue::MediaList;
use crate::imaging::ThumbnailSource;
use image::DynamicImage;
use std::collections::HashMap;
use std::sync::Arc;

/// A cached image and its associated metadata in the prefetch cache.
#[derive(Clone)]
pub struct CachedImage {
    /// The cached full-resolution decoded image, if loaded.
    pub image: Option<Arc<DynamicImage>>,
    /// The cached low-resolution thumbnail image, if loaded.
    pub thumbnail: Option<Arc<DynamicImage>>,
    /// Source of the thumbnail placeholder, if any.
    pub thumbnail_source: Option<ThumbnailSource>,
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
    /// Un-cropped native raw/sensor dimensions (`(width, height)`), if available.
    pub raw_dimensions: Option<(u32, u32)>,
}

/// Maximum capacity retained in the prefetch cache before shrinking down to initial capacity.
pub const MAX_PREFETCH_CACHE_CAPACITY: usize = 16;
/// Initial capacity allocated for the prefetch cache.
const INITIAL_PREFETCH_CACHE_CAPACITY: usize = 8;

/// Lock-free, main-thread prefetch cache storing decoded images and thumbnails for the visible sliding window.
#[derive(Default)]
pub struct PrefetchCache {
    entries: HashMap<usize, CachedImage>,
}

impl PrefetchCache {
    /// Creates a new empty `PrefetchCache` with initial capacity for the sliding window.
    pub fn new() -> Self {
        Self {
            entries: HashMap::with_capacity(INITIAL_PREFETCH_CACHE_CAPACITY),
        }
    }

    /// Returns a reference to the cached entry for `idx`, if present.
    pub fn get(&self, idx: usize) -> Option<&CachedImage> {
        self.entries.get(&idx)
    }

    /// Returns true if the cache contains an entry for `idx`.
    pub fn contains_key(&self, idx: usize) -> bool {
        self.entries.contains_key(&idx)
    }

    /// Inserts a cached entry for `idx`.
    #[cfg(test)]
    pub fn insert(&mut self, idx: usize, entry: CachedImage) {
        self.entries.insert(idx, entry);
    }

    /// Returns the number of cached entries.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns true if the cache contains no entries.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Clears all cached entries, shrinking capacity if it exceeded `MAX_PREFETCH_CACHE_CAPACITY`.
    pub fn clear(&mut self) {
        self.entries.clear();
        if self.entries.capacity() > MAX_PREFETCH_CACHE_CAPACITY {
            self.entries.shrink_to(INITIAL_PREFETCH_CACHE_CAPACITY);
        }
    }

    /// Retains only the entries present in the visible window, shrinking capacity if it exceeded `MAX_PREFETCH_CACHE_CAPACITY`.
    pub fn retain_window(&mut self, window: &VisibleWindow) {
        self.entries.retain(|idx, _| window.contains(*idx));
        if self.entries.capacity() > MAX_PREFETCH_CACHE_CAPACITY {
            self.entries.shrink_to(INITIAL_PREFETCH_CACHE_CAPACITY);
        }
    }

    /// Inserts or updates a thumbnail placeholder for `idx`.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_thumbnail(
        &mut self,
        idx: usize,
        thumb: Arc<DynamicImage>,
        thumbnail_source: Option<ThumbnailSource>,
        decode_duration: std::time::Duration,
        width: u32,
        height: u32,
        format: Option<image::ImageFormat>,
        disk_size: u64,
        raw_dimensions: Option<(u32, u32)>,
    ) {
        if let Some(cached) = self.entries.get_mut(&idx) {
            cached.thumbnail = Some(thumb);
            cached.thumbnail_source = thumbnail_source;
            cached.thumbnail_decode_duration = decode_duration;
            cached.raw_dimensions = raw_dimensions;
        } else {
            self.entries.insert(
                idx,
                CachedImage {
                    image: None,
                    thumbnail: Some(thumb),
                    thumbnail_source,
                    width,
                    height,
                    format,
                    decode_duration: std::time::Duration::ZERO,
                    thumbnail_decode_duration: decode_duration,
                    disk_size,
                    raw_dimensions,
                },
            );
        }
    }

    /// Inserts or updates a full-resolution decoded image for `idx`.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_full(
        &mut self,
        idx: usize,
        image: Arc<DynamicImage>,
        decode_duration: std::time::Duration,
        width: u32,
        height: u32,
        format: Option<image::ImageFormat>,
        disk_size: u64,
        raw_dimensions: Option<(u32, u32)>,
    ) {
        if let Some(cached) = self.entries.get_mut(&idx) {
            cached.image = Some(image);
            cached.width = width;
            cached.height = height;
            cached.format = format;
            cached.decode_duration = decode_duration;
            cached.disk_size = disk_size;
            cached.raw_dimensions = raw_dimensions;
        } else {
            self.entries.insert(
                idx,
                CachedImage {
                    image: Some(image),
                    thumbnail: None,
                    thumbnail_source: None,
                    width,
                    height,
                    format,
                    decode_duration,
                    thumbnail_decode_duration: std::time::Duration::ZERO,
                    disk_size,
                    raw_dimensions,
                },
            );
        }
    }

    /// Computes the number of cached entries and their total estimated memory footprint in bytes.
    pub fn stats(&self) -> (usize, u64) {
        let mut total_bytes = 0u64;
        for entry in self.entries.values() {
            if let Some(ref img) = entry.image {
                total_bytes = total_bytes.saturating_add(img.as_bytes().len() as u64);
            }
            if let Some(ref thumb) = entry.thumbnail {
                total_bytes = total_bytes.saturating_add(thumb.as_bytes().len() as u64);
            }
        }
        (self.entries.len(), total_bytes)
    }

    /// Returns the current allocation capacity of the underlying hash map.
    #[cfg(test)]
    pub fn capacity(&self) -> usize {
        self.entries.capacity()
    }
}

/// Zero-allocation, stack-allocated sliding window of up to 5 visible image indices (`current`, `±1`, `±2`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VisibleWindow {
    indices: [usize; 5],
    len: usize,
}

impl VisibleWindow {
    /// Returns the sliding window indices as a slice.
    #[inline]
    pub fn as_slice(&self) -> &[usize] {
        &self.indices[..self.len]
    }

    /// Returns true if the window contains the given index.
    #[inline]
    pub fn contains(&self, idx: usize) -> bool {
        self.as_slice().contains(&idx)
    }

    /// Returns true if the window contains no indices.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the number of indices in the window.
    #[inline]
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns an iterator over the indices in the window.
    #[inline]
    pub fn iter(&self) -> std::slice::Iter<'_, usize> {
        self.as_slice().iter()
    }

    /// Computes the sliding prefetch window for the active queue and view mode with zero heap allocations.
    pub fn compute(queue: &MediaList, view_mode: ViewMode) -> Self {
        let total_visible = queue.visible_count(view_mode);
        if total_visible == 0 || queue.is_empty() {
            return Self::default();
        }

        let current = queue.current_index;
        if !queue.is_visible(current, view_mode) {
            return Self::default();
        }

        // Gather up to 5 candidates: current, prev_1, prev_2, next_1, next_2
        let prev_1 = queue.prev_visible(current, view_mode).unwrap_or(current);
        let prev_2 = queue.prev_visible(prev_1, view_mode).unwrap_or(prev_1);
        let next_1 = queue.next_visible(current, view_mode).unwrap_or(current);
        let next_2 = queue.next_visible(next_1, view_mode).unwrap_or(next_1);

        let raw = [current, prev_1, next_1, prev_2, next_2];
        let mut indices = [0usize; 5];
        let mut len = 0;

        for &idx in &raw {
            if !indices[..len].contains(&idx) {
                indices[len] = idx;
                len += 1;
            }
        }

        indices[..len].sort_unstable();

        Self { indices, len }
    }
}

/// Helper function to compute sliding window indices as a heap-allocated vector.
#[cfg(test)]
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
    use crate::app::classifications::Classification;
    use crate::imaging::ImageSource;
    use std::path::PathBuf;

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

    #[test]
    fn test_visible_window_stack_computation() {
        let sources: Vec<ImageSource> = (0..5)
            .map(|i| ImageSource::Local(PathBuf::from(format!("{}.png", i))))
            .collect();
        let mut queue = MediaList::new(sources, 0).unwrap();

        // 1. All 5 visible, current 0
        let win = VisibleWindow::compute(&queue, ViewMode::All);
        assert_eq!(win.as_slice(), &[0, 1, 2, 3, 4]);
        assert_eq!(win.len(), 5);
        assert!(!win.is_empty());
        assert!(win.contains(0));
        assert!(win.contains(4));
        assert!(!win.contains(5));

        // 2. Filtered: index 1 and 3 hidden (marked Reject)
        queue.set_classification(1, Classification::Reject);
        queue.set_classification(3, Classification::Reject);
        queue.current_index = 2;

        let win = VisibleWindow::compute(&queue, ViewMode::Default);
        assert_eq!(win.as_slice(), &[0, 2, 4]);
        assert_eq!(win.len(), 3);
        assert!(win.contains(2));
        assert!(!win.contains(1));
        assert!(!win.contains(3));

        // 3. Single item queue
        let single_source = vec![ImageSource::Local(PathBuf::from("solo.png"))];
        let single_queue = MediaList::new(single_source, 0).unwrap();
        let win = VisibleWindow::compute(&single_queue, ViewMode::All);
        assert_eq!(win.as_slice(), &[0]);
        assert_eq!(win.len(), 1);

        // 4. Current index not visible -> empty window
        queue.current_index = 1; // 1 is Reject, not visible under Default
        let win = VisibleWindow::compute(&queue, ViewMode::Default);
        assert!(win.is_empty());
        assert_eq!(win.as_slice(), &[] as &[usize]);
    }

    #[test]
    fn test_prefetch_cache_operations_and_shrink() {
        let mut cache = PrefetchCache::new();
        assert_eq!(cache.len(), 0);
        assert!(cache.is_empty());

        let thumb = Arc::new(DynamicImage::ImageRgb8(image::RgbImage::new(10, 10)));
        cache.upsert_thumbnail(
            1,
            thumb,
            Some(ThumbnailSource::Exif),
            std::time::Duration::from_millis(2),
            100,
            100,
            Some(image::ImageFormat::Jpeg),
            1000,
            Some((1000, 1000)),
        );
        assert_eq!(cache.len(), 1);
        let entry = cache.get(1).expect("entry must exist");
        assert!(entry.image.is_none());
        assert!(entry.thumbnail.is_some());
        assert_eq!(entry.thumbnail_source, Some(ThumbnailSource::Exif));

        let full = Arc::new(DynamicImage::ImageRgb8(image::RgbImage::new(100, 100)));
        cache.upsert_full(
            1,
            full,
            std::time::Duration::from_millis(10),
            100,
            100,
            Some(image::ImageFormat::Jpeg),
            1000,
            Some((1000, 1000)),
        );
        assert_eq!(cache.len(), 1);
        let entry = cache.get(1).expect("entry must exist");
        assert!(entry.image.is_some());
        assert!(entry.thumbnail.is_some());

        // Insert more than MAX_PREFETCH_CACHE_CAPACITY items (e.g. 25 items) to expand capacity
        for i in 2..27 {
            let img = Arc::new(DynamicImage::ImageRgb8(image::RgbImage::new(5, 5)));
            cache.upsert_full(i, img, std::time::Duration::ZERO, 5, 5, None, 0, None);
        }
        assert!(cache.capacity() > MAX_PREFETCH_CACHE_CAPACITY);

        // Test retain_window shrinks capacity
        let sources: Vec<ImageSource> = (0..5)
            .map(|i| ImageSource::Local(PathBuf::from(format!("{}.png", i))))
            .collect();
        let queue = MediaList::new(sources, 0).unwrap();
        let window = VisibleWindow::compute(&queue, ViewMode::All);
        cache.retain_window(&window);
        assert!(cache.capacity() <= MAX_PREFETCH_CACHE_CAPACITY);

        // Re-expand capacity and test clear() shrinks capacity
        for i in 100..125 {
            let img = Arc::new(DynamicImage::ImageRgb8(image::RgbImage::new(5, 5)));
            cache.upsert_full(i, img, std::time::Duration::ZERO, 5, 5, None, 0, None);
        }
        assert!(cache.capacity() > MAX_PREFETCH_CACHE_CAPACITY);
        cache.clear();
        assert_eq!(cache.len(), 0);
        assert!(cache.capacity() <= MAX_PREFETCH_CACHE_CAPACITY);
    }
}
