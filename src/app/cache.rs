use crate::app::classifications::ViewMode;
use crate::app::queue::MediaList;
use crate::imaging::ThumbnailSource;
use crate::ui::ThumbnailState;
use image::DynamicImage;
use std::collections::HashMap;
use std::sync::Arc;

/// A cached low-resolution thumbnail image and its associated provenance and decode timing.
#[derive(Debug, Clone)]
pub struct CachedThumbnail {
    /// The cached low-resolution thumbnail image.
    pub image: Arc<DynamicImage>,
    /// Source of the thumbnail placeholder.
    pub source: ThumbnailSource,
    /// Time taken to decode the thumbnail image.
    pub decode_duration: std::time::Duration,
}

/// A cached full-resolution decoded image and its associated decode telemetry and file metadata.
#[derive(Debug, Clone)]
pub struct CachedFullImage {
    /// The cached full-resolution decoded image.
    pub image: Arc<DynamicImage>,
    /// Time taken to decode the full-resolution image.
    pub decode_duration: std::time::Duration,
    /// The image format (e.g., JPEG, PNG, etc.).
    pub format: image::ImageFormat,
    /// Size of the raw source file on disk in bytes.
    pub disk_size: u64,
}

/// A cached image and its associated metadata in the prefetch cache.
#[derive(Clone)]
pub struct CachedImage {
    /// The cached full-resolution decoded image and its metadata, if loaded.
    pub full: Option<CachedFullImage>,
    /// The cached low-resolution thumbnail placeholder or extraction state.
    pub thumbnail: ThumbnailState<CachedThumbnail>,
    /// The width of the image.
    pub width: u32,
    /// The height of the image.
    pub height: u32,
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

    /// Returns true if the cache contains a decoded full-resolution image for `idx`.
    pub fn has_full_image(&self, idx: usize) -> bool {
        self.entries.get(&idx).is_some_and(|e| e.full.is_some())
    }

    /// Returns true if the cache contains an entry for `idx`.
    #[cfg(test)]
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
    pub fn upsert_thumbnail(
        &mut self,
        idx: usize,
        thumbnail: CachedThumbnail,
        width: u32,
        height: u32,
        raw_dimensions: Option<(u32, u32)>,
    ) {
        if let Some(cached) = self.entries.get_mut(&idx) {
            cached.thumbnail = ThumbnailState::Loaded(thumbnail);
            if cached.full.is_none() {
                cached.width = width;
                cached.height = height;
                cached.raw_dimensions = raw_dimensions;
            }
        } else {
            self.entries.insert(
                idx,
                CachedImage {
                    full: None,
                    thumbnail: ThumbnailState::Loaded(thumbnail),
                    width,
                    height,
                    raw_dimensions,
                },
            );
        }
    }

    /// Inserts or updates a full-resolution decoded image for `idx`.
    pub fn upsert_full(
        &mut self,
        idx: usize,
        full: CachedFullImage,
        width: u32,
        height: u32,
        raw_dimensions: Option<(u32, u32)>,
        is_prefetch: bool,
    ) {
        if let Some(cached) = self.entries.get_mut(&idx) {
            if cached.full.is_none() && cached.thumbnail.is_absent() && is_prefetch {
                cached.thumbnail = ThumbnailState::Skipped;
            } else if !is_prefetch && cached.thumbnail.is_skipped() {
                cached.thumbnail = ThumbnailState::Absent;
            }
            cached.full = Some(full);
            cached.width = width;
            cached.height = height;
            cached.raw_dimensions = raw_dimensions;
        } else {
            self.entries.insert(
                idx,
                CachedImage {
                    full: Some(full),
                    thumbnail: if is_prefetch {
                        ThumbnailState::Skipped
                    } else {
                        ThumbnailState::Absent
                    },
                    width,
                    height,
                    raw_dimensions,
                },
            );
        }
    }

    /// Computes the number of cached entries and their total estimated memory footprint in bytes.
    pub fn stats(&self) -> (usize, u64) {
        let mut total_bytes = 0u64;
        for entry in self.entries.values() {
            if let Some(ref full) = entry.full {
                total_bytes = total_bytes.saturating_add(full.image.as_bytes().len() as u64);
            }
            if let ThumbnailState::Loaded(ref thumb) = entry.thumbnail {
                total_bytes = total_bytes.saturating_add(thumb.image.as_bytes().len() as u64);
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
        assert!(!cache.has_full_image(1));

        let thumb = Arc::new(DynamicImage::ImageRgb8(image::RgbImage::new(10, 10)));
        cache.upsert_thumbnail(
            1,
            CachedThumbnail {
                image: thumb,
                source: ThumbnailSource::Exif,
                decode_duration: std::time::Duration::from_millis(2),
            },
            100,
            100,
            Some((1000, 1000)),
        );
        assert_eq!(cache.len(), 1);
        let entry = cache.get(1).expect("entry must exist");
        assert!(entry.full.is_none());
        assert!(entry.thumbnail.is_loaded());
        assert!(!cache.has_full_image(1));
        assert!(cache.contains_key(1));
        assert_eq!(
            entry.thumbnail.loaded().map(|t| t.source),
            Some(ThumbnailSource::Exif)
        );

        let full = Arc::new(DynamicImage::ImageRgb8(image::RgbImage::new(100, 100)));
        cache.upsert_full(
            1,
            CachedFullImage {
                image: full,
                decode_duration: std::time::Duration::from_millis(10),
                format: image::ImageFormat::Jpeg,
                disk_size: 1000,
            },
            100,
            100,
            Some((1000, 1000)),
            false,
        );
        assert_eq!(cache.len(), 1);
        let entry = cache.get(1).expect("entry must exist");
        assert!(entry.full.is_some());
        assert!(entry.thumbnail.is_loaded());
        assert!(cache.has_full_image(1));
        assert!(cache.contains_key(1));

        // Insert more than MAX_PREFETCH_CACHE_CAPACITY items (e.g. 25 items) to expand capacity
        for i in 2..27 {
            let img = Arc::new(DynamicImage::ImageRgb8(image::RgbImage::new(5, 5)));
            cache.upsert_full(
                i,
                CachedFullImage {
                    image: img,
                    decode_duration: std::time::Duration::ZERO,
                    format: image::ImageFormat::Png,
                    disk_size: 0,
                },
                5,
                5,
                None,
                false,
            );
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
            cache.upsert_full(
                i,
                CachedFullImage {
                    image: img,
                    decode_duration: std::time::Duration::ZERO,
                    format: image::ImageFormat::Png,
                    disk_size: 0,
                },
                5,
                5,
                None,
                false,
            );
        }
        assert!(cache.capacity() > MAX_PREFETCH_CACHE_CAPACITY);
        cache.clear();
        assert_eq!(cache.len(), 0);
        assert!(cache.capacity() <= MAX_PREFETCH_CACHE_CAPACITY);
    }

    #[test]
    fn test_prefetch_cache_has_full_image() {
        let mut cache = PrefetchCache::new();

        // 1. Empty state
        assert!(!cache.has_full_image(0));
        assert!(!cache.contains_key(0));

        // 2. Thumbnail-only state
        let thumb = Arc::new(DynamicImage::ImageRgb8(image::RgbImage::new(10, 10)));
        cache.upsert_thumbnail(
            0,
            CachedThumbnail {
                image: thumb,
                source: ThumbnailSource::Exif,
                decode_duration: std::time::Duration::from_millis(2),
            },
            100,
            100,
            None,
        );
        assert!(cache.contains_key(0));
        assert!(!cache.has_full_image(0));

        // 3. Full-image state
        let full = Arc::new(DynamicImage::ImageRgb8(image::RgbImage::new(100, 100)));
        cache.upsert_full(
            0,
            CachedFullImage {
                image: full,
                decode_duration: std::time::Duration::from_millis(10),
                format: image::ImageFormat::Jpeg,
                disk_size: 1000,
            },
            100,
            100,
            None,
            false,
        );
        assert!(cache.contains_key(0));
        assert!(cache.has_full_image(0));
    }

    #[test]
    fn test_prefetch_cache_thumbnail_skipped() {
        let mut cache = PrefetchCache::new();

        // 1. Insert full image via foreground (is_prefetch = false): thumbnail state must be Absent
        let full1 = Arc::new(DynamicImage::ImageRgb8(image::RgbImage::new(50, 50)));
        cache.upsert_full(
            0,
            CachedFullImage {
                image: full1.clone(),
                decode_duration: std::time::Duration::from_millis(5),
                format: image::ImageFormat::Png,
                disk_size: 500,
            },
            50,
            50,
            None,
            false,
        );
        let entry0 = cache.get(0).expect("entry must exist");
        assert!(entry0.thumbnail.is_absent());

        // 2. Insert full image via background prefetch (is_prefetch = true): thumbnail state must be Skipped
        let full2 = Arc::new(DynamicImage::ImageRgb8(image::RgbImage::new(50, 50)));
        cache.upsert_full(
            1,
            CachedFullImage {
                image: full2,
                decode_duration: std::time::Duration::from_millis(5),
                format: image::ImageFormat::Png,
                disk_size: 500,
            },
            50,
            50,
            None,
            true,
        );
        let entry1 = cache.get(1).expect("entry must exist");
        assert!(entry1.thumbnail.is_skipped());

        // 3. Updating an existing entry that already has a thumbnail with is_prefetch = true:
        // thumbnail state must remain Loaded (thumbnail is already present)
        let thumb = Arc::new(DynamicImage::ImageRgb8(image::RgbImage::new(10, 10)));
        cache.upsert_thumbnail(
            2,
            CachedThumbnail {
                image: thumb,
                source: ThumbnailSource::Exif,
                decode_duration: std::time::Duration::from_millis(1),
            },
            100,
            100,
            None,
        );
        let full3 = Arc::new(DynamicImage::ImageRgb8(image::RgbImage::new(100, 100)));
        cache.upsert_full(
            2,
            CachedFullImage {
                image: full3,
                decode_duration: std::time::Duration::from_millis(10),
                format: image::ImageFormat::Jpeg,
                disk_size: 1000,
            },
            100,
            100,
            None,
            true,
        );
        let entry2 = cache.get(2).expect("entry must exist");
        assert!(entry2.thumbnail.is_loaded());

        // 4. Updating an entry that had ThumbnailState::Skipped with a thumbnail later:
        // thumbnail becomes Loaded
        cache.upsert_thumbnail(
            1,
            CachedThumbnail {
                image: Arc::new(DynamicImage::ImageRgb8(image::RgbImage::new(10, 10))),
                source: ThumbnailSource::Exif,
                decode_duration: std::time::Duration::from_millis(1),
            },
            50,
            50,
            None,
        );
        let entry1_updated = cache.get(1).expect("entry must exist");
        assert!(entry1_updated.thumbnail.is_loaded());

        // 5. Updating an existing foreground full-image entry (ThumbnailState::Absent)
        // with is_prefetch = true: thumbnail must remain Absent!
        cache.upsert_full(
            0,
            CachedFullImage {
                image: full1,
                decode_duration: std::time::Duration::from_millis(5),
                format: image::ImageFormat::Png,
                disk_size: 500,
            },
            50,
            50,
            None,
            true,
        );
        let entry0_updated = cache.get(0).expect("entry must exist");
        assert!(entry0_updated.thumbnail.is_absent());
    }

    #[test]
    fn test_upsert_thumbnail_metadata_preservation_when_full_image_loaded() {
        let mut cache = PrefetchCache::new();

        // 1. Initial upsert_thumbnail on empty entry (full is None) sets width, height, raw_dimensions
        cache.upsert_thumbnail(
            0,
            CachedThumbnail {
                image: Arc::new(DynamicImage::ImageRgb8(image::RgbImage::new(160, 120))),
                source: ThumbnailSource::Exif,
                decode_duration: std::time::Duration::from_millis(1),
            },
            160,
            120,
            Some((1920, 1080)),
        );
        let entry0 = cache.get(0).expect("entry must exist");
        assert!(entry0.full.is_none());
        assert!(entry0.thumbnail.is_loaded());
        assert_eq!(entry0.width, 160);
        assert_eq!(entry0.height, 120);
        assert_eq!(entry0.raw_dimensions, Some((1920, 1080)));

        // Updating thumbnail when full is still None updates metadata
        cache.upsert_thumbnail(
            0,
            CachedThumbnail {
                image: Arc::new(DynamicImage::ImageRgb8(image::RgbImage::new(320, 240))),
                source: ThumbnailSource::XdgCache,
                decode_duration: std::time::Duration::from_millis(2),
            },
            320,
            240,
            Some((3840, 2160)),
        );
        let entry0_updated = cache.get(0).expect("entry must exist");
        assert_eq!(entry0_updated.width, 320);
        assert_eq!(entry0_updated.height, 240);
        assert_eq!(entry0_updated.raw_dimensions, Some((3840, 2160)));

        // 2. Full image decode completes: entry1 has full = Some(...) and full-resolution metadata
        let full_img = Arc::new(DynamicImage::ImageRgb8(image::RgbImage::new(4000, 3000)));
        cache.upsert_full(
            1,
            CachedFullImage {
                image: full_img,
                decode_duration: std::time::Duration::from_millis(20),
                format: image::ImageFormat::Tiff,
                disk_size: 50_000_000,
            },
            4000,
            3000,
            Some((4000, 3000)),
            false,
        );
        let entry1 = cache.get(1).expect("entry must exist");
        assert!(entry1.full.is_some());
        assert!(entry1.thumbnail.is_absent());
        assert_eq!(entry1.width, 4000);
        assert_eq!(entry1.height, 3000);
        let full1 = entry1.full.as_ref().unwrap();
        assert_eq!(full1.format, image::ImageFormat::Tiff);
        assert_eq!(full1.disk_size, 50_000_000);
        assert_eq!(entry1.raw_dimensions, Some((4000, 3000)));

        // 3. Late-arriving thumbnail for entry1:
        // Must attach thumbnail as Loaded, but must NOT overwrite width, height, or raw_dimensions
        cache.upsert_thumbnail(
            1,
            CachedThumbnail {
                image: Arc::new(DynamicImage::ImageRgb8(image::RgbImage::new(160, 120))),
                source: ThumbnailSource::Exif,
                decode_duration: std::time::Duration::from_millis(2),
            },
            160,
            120,
            Some((160, 120)),
        );
        let entry1_after_thumb = cache.get(1).expect("entry must exist");
        assert!(entry1_after_thumb.full.is_some());
        assert!(entry1_after_thumb.thumbnail.is_loaded());
        assert_eq!(entry1_after_thumb.width, 4000);
        assert_eq!(entry1_after_thumb.height, 3000);
        let full_after = entry1_after_thumb.full.as_ref().unwrap();
        assert_eq!(full_after.format, image::ImageFormat::Tiff);
        assert_eq!(full_after.disk_size, 50_000_000);
        assert_eq!(entry1_after_thumb.raw_dimensions, Some((4000, 3000)));
    }
}
