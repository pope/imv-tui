use crate::app::adjustments::ImageAdjustments;
use crate::app::classifications::{Classification, ViewMode};
use crate::imaging::ImageSource;
use std::collections::HashMap;

/// An interactive queue containing the loaded images, relative path caches, classifications, adjustments, and bitset filters.
pub struct ImageQueue {
    /// Loaded list of image sources.
    images: Vec<ImageSource>,
    /// Common prefix stripped relative paths for search.
    relative_paths: Vec<String>,
    /// Lowercase relative paths for case-insensitive matching.
    relative_paths_lowercase: Vec<String>,
    /// Per-image classification flags (Pick, Reject, Unflagged).
    classifications: Vec<Classification>,
    /// Per-image adjustment settings (brightness, contrast, rotation).
    adjustments: Vec<ImageAdjustments>,
    /// 64-bit bitset tracking images marked as Pick (1 bit per image).
    picks_bitset: Vec<u64>,
    /// 64-bit bitset tracking images marked as Reject (1 bit per image).
    rejects_bitset: Vec<u64>,
    /// Fast O(1) total count of images marked as Pick.
    pick_count: usize,
    /// Fast O(1) total count of images marked as Reject.
    reject_count: usize,
    /// Current selected index in the images vector.
    pub current_index: usize,
}

/// Type alias for ImageQueue representing the encapsulated media collection.
pub(crate) type MediaList = ImageQueue;

impl ImageQueue {
    /// Computes common prefix stripped relative paths and their lowercase counterparts.
    fn compute_relative_paths(images: &[ImageSource]) -> (Vec<String>, Vec<String>) {
        if images.is_empty() {
            return (Vec::new(), Vec::new());
        }

        let mut first_path = match &images[0] {
            ImageSource::Local(p) => p.parent().unwrap_or(p).to_path_buf(),
            ImageSource::Cbz { zip_path, .. } => {
                zip_path.parent().unwrap_or(zip_path).to_path_buf()
            }
        };
        for img in &images[1..] {
            let p = match img {
                ImageSource::Local(p) => p.parent().unwrap_or(p).to_path_buf(),
                ImageSource::Cbz { zip_path, .. } => {
                    zip_path.parent().unwrap_or(zip_path).to_path_buf()
                }
            };
            while !p.starts_with(&first_path) {
                if !first_path.pop() {
                    break;
                }
            }
        }
        let common_prefix = first_path;

        let relative_paths: Vec<String> = images
            .iter()
            .map(|img| match img {
                ImageSource::Local(path) => {
                    if let Ok(rel) = path.strip_prefix(&common_prefix) {
                        rel.to_string_lossy().to_string()
                    } else {
                        path.to_string_lossy().to_string()
                    }
                }
                ImageSource::Cbz {
                    zip_path,
                    file_in_zip,
                } => {
                    if let Ok(rel_zip) = zip_path.strip_prefix(&common_prefix) {
                        format!("{}: {}", rel_zip.to_string_lossy(), file_in_zip)
                    } else {
                        let zip_name = zip_path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("Unknown");
                        format!("{}: {}", zip_name, file_in_zip)
                    }
                }
            })
            .collect();

        let relative_paths_lowercase: Vec<String> = relative_paths
            .iter()
            .map(|name| name.to_lowercase())
            .collect();

        (relative_paths, relative_paths_lowercase)
    }

    /// Creates a new ImageQueue with unflagged classifications and default adjustments.
    pub fn new(images: Vec<ImageSource>, current_index: usize) -> Result<Self, String> {
        if images.is_empty() {
            return Err("No supported images found".to_string());
        }

        let len = images.len();
        let (relative_paths, relative_paths_lowercase) = Self::compute_relative_paths(&images);
        let classifications = vec![Classification::Unflagged; len];
        let adjustments = vec![ImageAdjustments::default(); len];
        let num_words = len.div_ceil(64);
        let picks_bitset = vec![0u64; num_words];
        let rejects_bitset = vec![0u64; num_words];
        let current_index = current_index.min(len - 1);

        let queue = Self {
            images,
            relative_paths,
            relative_paths_lowercase,
            classifications,
            adjustments,
            picks_bitset,
            rejects_bitset,
            pick_count: 0,
            reject_count: 0,
            current_index,
        };
        queue.debug_assert_invariants();
        Ok(queue)
    }

    /// Creates an ImageQueue with pre-existing classifications and adjustments.
    #[cfg(test)]
    pub(crate) fn with_state(
        images: Vec<ImageSource>,
        current_index: usize,
        classifications: Vec<Classification>,
        adjustments: Vec<ImageAdjustments>,
    ) -> Result<Self, String> {
        if images.is_empty() {
            return Err("No supported images found".to_string());
        }
        if classifications.len() != images.len() {
            return Err("Classifications length mismatch".to_string());
        }
        if adjustments.len() != images.len() {
            return Err("Adjustments length mismatch".to_string());
        }

        let len = images.len();
        let (relative_paths, relative_paths_lowercase) = Self::compute_relative_paths(&images);
        let num_words = len.div_ceil(64);
        let mut picks_bitset = vec![0u64; num_words];
        let mut rejects_bitset = vec![0u64; num_words];
        let mut pick_count = 0;
        let mut reject_count = 0;

        for (idx, &class) in classifications.iter().enumerate() {
            match class {
                Classification::Pick => {
                    picks_bitset[idx / 64] |= 1u64 << (idx % 64);
                    pick_count += 1;
                }
                Classification::Reject => {
                    rejects_bitset[idx / 64] |= 1u64 << (idx % 64);
                    reject_count += 1;
                }
                Classification::Unflagged => {}
            }
        }

        let current_index = current_index.min(len - 1);
        let queue = Self {
            images,
            relative_paths,
            relative_paths_lowercase,
            classifications,
            adjustments,
            picks_bitset,
            rejects_bitset,
            pick_count,
            reject_count,
            current_index,
        };
        queue.debug_assert_invariants();
        Ok(queue)
    }

    /// Asserts all internal vector lengths and bitset dimensions match exactly.
    #[inline]
    fn debug_assert_invariants(&self) {
        debug_assert_eq!(self.images.len(), self.relative_paths.len());
        debug_assert_eq!(self.images.len(), self.relative_paths_lowercase.len());
        debug_assert_eq!(self.images.len(), self.classifications.len());
        debug_assert_eq!(self.images.len(), self.adjustments.len());
        let expected_words = self.images.len().div_ceil(64);
        debug_assert_eq!(self.picks_bitset.len(), expected_words);
        debug_assert_eq!(self.rejects_bitset.len(), expected_words);
        if !self.images.is_empty() {
            debug_assert!(self.current_index < self.images.len());
        }
    }

    /// Replaces the current images with `new_images`, preserving classifications and adjustments
    /// for existing files by their canonical identifier and migrating removed non-default items
    /// to `unmapped`.
    pub(crate) fn rescan_preserving_state(
        &mut self,
        new_images: Vec<ImageSource>,
        unmapped: &mut HashMap<String, (Classification, ImageAdjustments)>,
    ) -> Result<(), String> {
        if new_images.is_empty() {
            return Err("No supported images found".to_string());
        }

        let cwd = std::env::current_dir().ok();

        // Get the current selected image identifier, if any, to preserve the position
        let current_ident = self
            .images
            .get(self.current_index)
            .map(|img| img.identifier_with_cwd(cwd.as_deref()));

        // Build mapping from identifier to (classification, adjustments) from the old queue
        let mut old_mapping = HashMap::with_capacity(self.images.len());
        for (idx, img) in self.images.iter().enumerate() {
            let ident = img.identifier_with_cwd(cwd.as_deref());
            let class = self.classifications[idx];
            let adj = self.adjustments[idx];
            old_mapping.insert(ident, (class, adj));
        }

        // Prepare new vectors
        let num_new_images = new_images.len();
        let mut new_classifications = vec![Classification::Unflagged; num_new_images];
        let mut new_adjustments = vec![ImageAdjustments::default(); num_new_images];

        // Match new images against old mapping and unmapped
        for (idx, img) in new_images.iter().enumerate() {
            let ident = img.identifier_with_cwd(cwd.as_deref());
            if let Some((class, adj)) = old_mapping.remove(&ident) {
                new_classifications[idx] = class;
                new_adjustments[idx] = adj;
            } else if let Some((class, adj)) = unmapped.remove(&ident) {
                new_classifications[idx] = class;
                new_adjustments[idx] = adj;
            }
        }

        // Move remaining old items that were flagged/adjusted to unmapped
        for (ident, (class, adj)) in old_mapping {
            if class != Classification::Unflagged || adj != ImageAdjustments::default() {
                unmapped.insert(ident, (class, adj));
            }
        }

        // Find the index of the previously selected image in the new queue
        let new_index = if let Some(ref target_ident) = current_ident {
            new_images
                .iter()
                .position(|img| img.identifier_with_cwd(cwd.as_deref()) == *target_ident)
                .unwrap_or_else(|| self.current_index.min(num_new_images - 1))
        } else {
            0
        };

        // Construct new synchronized state
        let (relative_paths, relative_paths_lowercase) = Self::compute_relative_paths(&new_images);
        let num_words = num_new_images.div_ceil(64);
        let mut picks_bitset = vec![0u64; num_words];
        let mut rejects_bitset = vec![0u64; num_words];
        let mut pick_count = 0;
        let mut reject_count = 0;

        for (idx, &class) in new_classifications.iter().enumerate() {
            let word = idx / 64;
            let bit = idx % 64;
            match class {
                Classification::Pick => {
                    picks_bitset[word] |= 1u64 << bit;
                    pick_count += 1;
                }
                Classification::Reject => {
                    rejects_bitset[word] |= 1u64 << bit;
                    reject_count += 1;
                }
                Classification::Unflagged => {}
            }
        }

        self.images = new_images;
        self.relative_paths = relative_paths;
        self.relative_paths_lowercase = relative_paths_lowercase;
        self.classifications = new_classifications;
        self.adjustments = new_adjustments;
        self.picks_bitset = picks_bitset;
        self.rejects_bitset = rejects_bitset;
        self.pick_count = pick_count;
        self.reject_count = reject_count;
        self.current_index = new_index;

        self.debug_assert_invariants();
        Ok(())
    }

    /// Returns the number of images in the queue.
    #[inline]
    pub fn len(&self) -> usize {
        self.images.len()
    }

    /// Returns true if the image queue contains no images.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    /// Returns a slice of the loaded image sources.
    #[inline]
    pub fn images(&self) -> &[ImageSource] {
        &self.images
    }

    /// Returns a slice of relative paths for display and search.
    #[inline]
    pub fn relative_paths(&self) -> &[String] {
        &self.relative_paths
    }

    /// Returns a slice of lowercase relative paths for matching.
    #[inline]
    pub fn relative_paths_lowercase(&self) -> &[String] {
        &self.relative_paths_lowercase
    }

    /// Returns a slice of classifications.
    #[inline]
    pub fn classifications(&self) -> &[Classification] {
        &self.classifications
    }

    /// Returns the classification of the image at `idx`, or `Unflagged` if out of bounds.
    #[inline]
    pub fn classification(&self, idx: usize) -> Classification {
        self.classifications
            .get(idx)
            .copied()
            .unwrap_or(Classification::Unflagged)
    }

    /// Returns a slice of image adjustments.
    #[inline]
    pub fn adjustments(&self) -> &[ImageAdjustments] {
        &self.adjustments
    }

    /// Returns a mutable slice of image adjustments for in-place modification.
    #[inline]
    pub fn adjustments_mut(&mut self) -> &mut [ImageAdjustments] {
        &mut self.adjustments
    }

    /// Returns the adjustment settings for the image at `idx`, or default if out of bounds.
    #[inline]
    pub fn adjustment(&self, idx: usize) -> ImageAdjustments {
        self.adjustments.get(idx).copied().unwrap_or_default()
    }

    /// Sets the classification of the image at `idx`, atomically updating bitsets and counters.
    pub fn set_classification(&mut self, idx: usize, new_class: Classification) {
        if idx >= self.images.len() {
            return;
        }
        let old_class = self.classifications[idx];
        if old_class == new_class {
            return;
        }

        let word_idx = idx / 64;
        let bit_mask = 1u64 << (idx % 64);

        // Clear old bit and update counter
        match old_class {
            Classification::Pick => {
                self.picks_bitset[word_idx] &= !bit_mask;
                self.pick_count = self.pick_count.saturating_sub(1);
            }
            Classification::Reject => {
                self.rejects_bitset[word_idx] &= !bit_mask;
                self.reject_count = self.reject_count.saturating_sub(1);
            }
            Classification::Unflagged => {}
        }

        // Set new bit and update counter
        match new_class {
            Classification::Pick => {
                self.picks_bitset[word_idx] |= bit_mask;
                self.pick_count += 1;
            }
            Classification::Reject => {
                self.rejects_bitset[word_idx] |= bit_mask;
                self.reject_count += 1;
            }
            Classification::Unflagged => {}
        }

        self.classifications[idx] = new_class;
    }

    /// Resets all classifications to Unflagged, adjustments to default, and clears bitsets.
    #[cfg(test)]
    pub(crate) fn clear_classifications_and_adjustments(&mut self) {
        self.classifications.fill(Classification::Unflagged);
        for adj in &mut self.adjustments {
            *adj = ImageAdjustments::default();
        }
        self.picks_bitset.fill(0);
        self.rejects_bitset.fill(0);
        self.pick_count = 0;
        self.reject_count = 0;
    }

    /// Appends an item to all 5 vectors and updates bitsets and counters atomically.
    #[cfg(test)]
    pub(crate) fn push(
        &mut self,
        source: ImageSource,
        relative_path: String,
        classification: Classification,
        adjustment: ImageAdjustments,
    ) {
        let idx = self.images.len();
        self.images.push(source);
        self.relative_paths_lowercase
            .push(relative_path.to_lowercase());
        self.relative_paths.push(relative_path);
        self.classifications.push(classification);
        self.adjustments.push(adjustment);

        let word_idx = idx / 64;
        if word_idx >= self.picks_bitset.len() {
            self.picks_bitset.push(0);
            self.rejects_bitset.push(0);
        }

        let bit_mask = 1u64 << (idx % 64);
        match classification {
            Classification::Pick => {
                self.picks_bitset[word_idx] |= bit_mask;
                self.pick_count += 1;
            }
            Classification::Reject => {
                self.rejects_bitset[word_idx] |= bit_mask;
                self.reject_count += 1;
            }
            Classification::Unflagged => {}
        }
        self.debug_assert_invariants();
    }

    /// Removes an item at `idx` via swap_remove across all 5 vectors, maintaining bitsets and counters.
    #[cfg(test)]
    pub(crate) fn swap_remove(&mut self, idx: usize) {
        if idx >= self.images.len() {
            return;
        }
        let last_idx = self.images.len() - 1;
        let old_idx_class = self.classifications[idx];
        let last_class = self.classifications[last_idx];

        // 1. Decrement counter for the removed element at idx
        match old_idx_class {
            Classification::Pick => self.pick_count = self.pick_count.saturating_sub(1),
            Classification::Reject => self.reject_count = self.reject_count.saturating_sub(1),
            Classification::Unflagged => {}
        }

        // 2. Clear bit for last_idx
        let last_word = last_idx / 64;
        let last_mask = 1u64 << (last_idx % 64);
        self.picks_bitset[last_word] &= !last_mask;
        self.rejects_bitset[last_word] &= !last_mask;

        // 3. If idx != last_idx, set bit at idx according to last_class
        if idx != last_idx {
            let word_idx = idx / 64;
            let bit_mask = 1u64 << (idx % 64);
            self.picks_bitset[word_idx] &= !bit_mask;
            self.rejects_bitset[word_idx] &= !bit_mask;
            match last_class {
                Classification::Pick => self.picks_bitset[word_idx] |= bit_mask,
                Classification::Reject => self.rejects_bitset[word_idx] |= bit_mask,
                Classification::Unflagged => {}
            }
        }

        // 4. Perform swap_remove on all 5 vectors
        self.images.swap_remove(idx);
        self.relative_paths.swap_remove(idx);
        self.relative_paths_lowercase.swap_remove(idx);
        self.classifications.swap_remove(idx);
        self.adjustments.swap_remove(idx);

        // 5. Truncate bitsets if word count decreased
        let new_len = self.images.len();
        let expected_words = new_len.div_ceil(64);
        self.picks_bitset.truncate(expected_words);
        self.rejects_bitset.truncate(expected_words);
        if self.current_index >= new_len && new_len > 0 {
            self.current_index = new_len - 1;
        }
        self.debug_assert_invariants();
    }

    /// Returns the 64-bit visibility bitmask for word `word_idx` under `view_mode`, with out-of-bounds bits masked out.
    #[inline]
    fn get_view_word(&self, word_idx: usize, view_mode: ViewMode) -> u64 {
        if word_idx >= self.picks_bitset.len() {
            return 0;
        }
        let raw_word = match view_mode {
            ViewMode::Default => !self.rejects_bitset[word_idx],
            ViewMode::Unflagged => !(self.picks_bitset[word_idx] | self.rejects_bitset[word_idx]),
            ViewMode::Picks => self.picks_bitset[word_idx],
            ViewMode::Rejects => self.rejects_bitset[word_idx],
            ViewMode::All => !0u64,
        };

        let mask = if word_idx + 1 < self.picks_bitset.len() {
            !0u64
        } else {
            let rem = self.images.len() % 64;
            if rem == 0 {
                !0u64
            } else {
                (!0u64) >> (64 - rem)
            }
        };

        raw_word & mask
    }

    /// Returns true if the image at `idx` is visible under `view_mode`.
    #[inline]
    pub fn is_visible(&self, idx: usize, view_mode: ViewMode) -> bool {
        if idx >= self.images.len() {
            return false;
        }
        let word_idx = idx / 64;
        let bit_idx = idx % 64;
        let word = self.get_view_word(word_idx, view_mode);
        (word & (1u64 << bit_idx)) != 0
    }

    /// Returns the total number of visible images under `view_mode` in O(1) branchless arithmetic.
    #[inline]
    pub fn visible_count(&self, view_mode: ViewMode) -> usize {
        let total = self.images.len();
        match view_mode {
            ViewMode::Default => total.saturating_sub(self.reject_count),
            ViewMode::Unflagged => total.saturating_sub(self.pick_count + self.reject_count),
            ViewMode::Picks => self.pick_count,
            ViewMode::Rejects => self.reject_count,
            ViewMode::All => total,
        }
    }

    /// Returns the next visible index after `from_idx`, wrapping around if needed.
    pub fn next_visible(&self, from_idx: usize, view_mode: ViewMode) -> Option<usize> {
        let total = self.images.len();
        if total == 0 || self.visible_count(view_mode) == 0 {
            return None;
        }

        let num_words = self.picks_bitset.len();
        let from_idx = from_idx.min(total - 1);

        // 1. Scan forward from `from_idx + 1` to `total - 1`
        if from_idx + 1 < total {
            let start_idx = from_idx + 1;
            let start_word = start_idx / 64;
            let start_bit = start_idx % 64;

            // Check first partial word
            let mut word = self.get_view_word(start_word, view_mode);
            word &= (!0u64) << start_bit;
            if word != 0 {
                let bit = word.trailing_zeros() as usize;
                let idx = start_word * 64 + bit;
                if idx < total {
                    return Some(idx);
                }
            }

            // Check remaining words forward
            for w in (start_word + 1)..num_words {
                let word = self.get_view_word(w, view_mode);
                if word != 0 {
                    let bit = word.trailing_zeros() as usize;
                    let idx = w * 64 + bit;
                    if idx < total {
                        return Some(idx);
                    }
                }
            }
        }

        // 2. Wrap around and scan from 0 up to `from_idx`
        let end_word = from_idx / 64;
        let end_bit = from_idx % 64;

        // Check full words before end_word
        for w in 0..end_word {
            let word = self.get_view_word(w, view_mode);
            if word != 0 {
                let bit = word.trailing_zeros() as usize;
                let idx = w * 64 + bit;
                if idx < total {
                    return Some(idx);
                }
            }
        }

        // Check last partial word up to end_bit
        let mut word = self.get_view_word(end_word, view_mode);
        let mask = if end_bit == 63 {
            !0u64
        } else {
            (1u64 << (end_bit + 1)) - 1
        };
        word &= mask;
        if word != 0 {
            let bit = word.trailing_zeros() as usize;
            let idx = end_word * 64 + bit;
            if idx < total {
                return Some(idx);
            }
        }

        None
    }

    /// Returns the previous visible index before `from_idx`, wrapping around if needed.
    pub fn prev_visible(&self, from_idx: usize, view_mode: ViewMode) -> Option<usize> {
        let total = self.images.len();
        if total == 0 || self.visible_count(view_mode) == 0 {
            return None;
        }

        let from_idx = from_idx.min(total - 1);

        // 1. Scan backward from `from_idx - 1` down to 0
        if from_idx > 0 {
            let target_idx = from_idx - 1;
            let target_word = target_idx / 64;
            let target_bit = target_idx % 64;

            // Check first partial word in reverse
            let mut word = self.get_view_word(target_word, view_mode);
            let mask = if target_bit == 63 {
                !0u64
            } else {
                (1u64 << (target_bit + 1)) - 1
            };
            word &= mask;
            if word != 0 {
                let bit = 63 - word.leading_zeros() as usize;
                let idx = target_word * 64 + bit;
                if idx < total {
                    return Some(idx);
                }
            }

            // Check remaining words down to 0
            for w in (0..target_word).rev() {
                let word = self.get_view_word(w, view_mode);
                if word != 0 {
                    let bit = 63 - word.leading_zeros() as usize;
                    let idx = w * 64 + bit;
                    if idx < total {
                        return Some(idx);
                    }
                }
            }
        }

        // 2. Wrap around and scan from `total - 1` down to `from_idx`
        let start_idx = total - 1;
        let start_word = start_idx / 64;
        let from_word = from_idx / 64;
        let from_bit = from_idx % 64;

        // Check full words down to from_word + 1
        for w in ((from_word + 1)..=start_word).rev() {
            let word = self.get_view_word(w, view_mode);
            if word != 0 {
                let bit = 63 - word.leading_zeros() as usize;
                let idx = w * 64 + bit;
                if idx < total {
                    return Some(idx);
                }
            }
        }

        // Check last partial word down to from_bit
        let mut word = self.get_view_word(from_word, view_mode);
        word &= (!0u64) << from_bit;
        if word != 0 {
            let bit = 63 - word.leading_zeros() as usize;
            let idx = from_word * 64 + bit;
            if idx < total {
                return Some(idx);
            }
        }

        None
    }

    /// Returns the closest visible index starting from `from_idx` (checking `from_idx`, then scanning forward).
    pub fn find_closest_visible(&self, from_idx: usize, view_mode: ViewMode) -> Option<usize> {
        if self.is_visible(from_idx, view_mode) {
            Some(from_idx)
        } else {
            self.next_visible(from_idx, view_mode)
        }
    }

    /// Returns the first visible index in the queue under `view_mode`.
    #[cfg(test)]
    pub(crate) fn first_visible(&self, view_mode: ViewMode) -> Option<usize> {
        let total = self.images.len();
        if total == 0 || self.visible_count(view_mode) == 0 {
            return None;
        }
        for w in 0..self.picks_bitset.len() {
            let word = self.get_view_word(w, view_mode);
            if word != 0 {
                let idx = w * 64 + word.trailing_zeros() as usize;
                if idx < total {
                    return Some(idx);
                }
            }
        }
        None
    }

    /// Returns the last visible index in the queue under `view_mode`.
    #[cfg(test)]
    pub(crate) fn last_visible(&self, view_mode: ViewMode) -> Option<usize> {
        let total = self.images.len();
        if total == 0 || self.visible_count(view_mode) == 0 {
            return None;
        }
        for w in (0..self.picks_bitset.len()).rev() {
            let word = self.get_view_word(w, view_mode);
            if word != 0 {
                let bit = 63 - word.leading_zeros() as usize;
                let idx = w * 64 + bit;
                if idx < total {
                    return Some(idx);
                }
            }
        }
        None
    }

    /// Returns the 0-based index of `idx` among visible items, or None if `idx` is not visible.
    pub fn visible_position(&self, idx: usize, view_mode: ViewMode) -> Option<usize> {
        if !self.is_visible(idx, view_mode) {
            return None;
        }
        let word_idx = idx / 64;
        let bit_idx = idx % 64;

        let mut count = 0;
        for w in 0..word_idx {
            count += self.get_view_word(w, view_mode).count_ones() as usize;
        }

        let word = self.get_view_word(word_idx, view_mode);
        let mask = if bit_idx == 63 {
            !0u64
        } else {
            (1u64 << (bit_idx + 1)) - 1
        };
        let partial = (word & mask).count_ones() as usize;
        count += partial;

        Some(count.saturating_sub(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imaging::types::{Brightness, Contrast, Rotation};
    use std::path::PathBuf;

    #[test]
    fn test_common_prefix_stripping() {
        let images = vec![
            ImageSource::Local(PathBuf::from("/home/user/Pictures/2026/vacation/lake.png")),
            ImageSource::Local(PathBuf::from(
                "/home/user/Pictures/2026/vacation/mountains.png",
            )),
            ImageSource::Local(PathBuf::from("/home/user/Pictures/2026/work/office.png")),
        ];
        let queue = ImageQueue::new(images, 0).unwrap();

        assert_eq!(queue.relative_paths()[0], "vacation/lake.png");
        assert_eq!(queue.relative_paths()[1], "vacation/mountains.png");
        assert_eq!(queue.relative_paths()[2], "work/office.png");
    }

    #[test]
    fn test_image_queue_no_common_prefix() {
        let images = vec![
            ImageSource::Local(PathBuf::from("/a/first.png")),
            ImageSource::Local(PathBuf::from("/b/second.png")),
        ];
        let queue = ImageQueue::new(images, 0).unwrap();
        assert_eq!(queue.relative_paths()[0], "a/first.png");
        assert_eq!(queue.relative_paths()[1], "b/second.png");
    }

    #[test]
    fn test_image_queue_single_file() {
        let images = vec![ImageSource::Local(PathBuf::from("/home/user/pic.jpg"))];
        let queue = ImageQueue::new(images, 0).unwrap();
        assert_eq!(queue.relative_paths()[0], "pic.jpg");
    }

    #[test]
    fn test_image_queue_current_index_out_of_bounds() {
        let images = vec![
            ImageSource::Local(PathBuf::from("1.jpg")),
            ImageSource::Local(PathBuf::from("2.jpg")),
        ];
        let queue = ImageQueue::new(images, 5).unwrap();
        assert_eq!(queue.current_index, 1);
    }

    #[test]
    fn test_media_list_invariants_push_swap_remove_rescan() {
        let sources = vec![
            ImageSource::Local(PathBuf::from("test1.png")),
            ImageSource::Local(PathBuf::from("test2.png")),
            ImageSource::Local(PathBuf::from("test3.png")),
        ];
        let mut queue = MediaList::new(sources, 0).unwrap();
        assert_eq!(queue.len(), 3);
        assert_eq!(queue.visible_count(ViewMode::All), 3);
        assert_eq!(queue.visible_count(ViewMode::Picks), 0);
        assert_eq!(queue.visible_count(ViewMode::Rejects), 0);

        // Mutate classification
        queue.set_classification(0, Classification::Pick);
        queue.set_classification(1, Classification::Reject);
        assert_eq!(queue.classification(0), Classification::Pick);
        assert_eq!(queue.classification(1), Classification::Reject);
        assert_eq!(queue.classification(2), Classification::Unflagged);
        assert_eq!(queue.visible_count(ViewMode::Picks), 1);
        assert_eq!(queue.visible_count(ViewMode::Rejects), 1);
        assert_eq!(queue.visible_count(ViewMode::Default), 2); // Pick + Unflagged

        // Push new item
        queue.push(
            ImageSource::Local(PathBuf::from("test4.png")),
            "test4.png".to_string(),
            Classification::Pick,
            ImageAdjustments {
                brightness: Brightness::new(10),
                contrast: Contrast::new(5.0),
                rotation: Rotation::D90,
            },
        );
        assert_eq!(queue.len(), 4);
        assert_eq!(queue.visible_count(ViewMode::Picks), 2);
        assert_eq!(queue.adjustment(3).brightness, Brightness::new(10));

        // swap_remove element 0 (which was Pick)
        queue.swap_remove(0);
        assert_eq!(queue.len(), 3);
        // Element at index 0 is now test4.png (was at index 3, which was Pick)
        assert_eq!(queue.classification(0), Classification::Pick);
        assert_eq!(queue.visible_count(ViewMode::Picks), 1);

        // swap_remove element 1 (which was Reject)
        queue.swap_remove(1);
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.visible_count(ViewMode::Rejects), 0);

        // with_state verification
        let reloaded = MediaList::with_state(
            queue.images().to_vec(),
            queue.current_index,
            queue.classifications().to_vec(),
            queue.adjustments().to_vec(),
        )
        .unwrap();
        assert_eq!(reloaded.len(), queue.len());
        assert_eq!(
            reloaded.visible_count(ViewMode::Picks),
            queue.visible_count(ViewMode::Picks)
        );

        // rescan_preserving_state verification
        let mut unmapped = HashMap::new();
        let new_sources = vec![
            ImageSource::Local(PathBuf::from("test4.png")),
            ImageSource::Local(PathBuf::from("test5.png")),
        ];
        queue
            .rescan_preserving_state(new_sources, &mut unmapped)
            .unwrap();
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.classification(0), Classification::Pick);
        assert_eq!(queue.adjustment(0).brightness, Brightness::new(10));
        assert_eq!(queue.classification(1), Classification::Unflagged);
        assert_eq!(queue.visible_count(ViewMode::Picks), 1);
        assert_eq!(queue.visible_count(ViewMode::Default), 2);
    }

    #[test]
    fn test_bitset_navigation_equivalence_with_linear_scan() {
        // Create 200 items spanning 4 64-bit words (0..64, 64..128, 128..192, 192..200)
        let total = 200;
        let sources: Vec<ImageSource> = (0..total)
            .map(|i| ImageSource::Local(PathBuf::from(format!("img_{:03}.png", i))))
            .collect();
        let mut queue = MediaList::new(sources, 0).unwrap();

        // Flag specific items across boundaries
        // 0, 63, 64, 127, 128, 199
        let picks = [0, 63, 128, 199];
        let rejects = [10, 64, 127, 150];
        for &idx in &picks {
            queue.set_classification(idx, Classification::Pick);
        }
        for &idx in &rejects {
            queue.set_classification(idx, Classification::Reject);
        }

        let view_modes = [
            ViewMode::Default,
            ViewMode::Unflagged,
            ViewMode::Picks,
            ViewMode::Rejects,
            ViewMode::All,
        ];

        for &mode in &view_modes {
            // Reference linear list of visible indices
            let linear_visible: Vec<usize> = (0..total)
                .filter(|&idx| {
                    crate::app::classifications::is_image_visible(
                        idx,
                        queue.classifications(),
                        mode,
                    )
                })
                .collect();

            // 1. Assert visible_count matches linear scan
            assert_eq!(
                queue.visible_count(mode),
                linear_visible.len(),
                "visible_count mismatch for {:?}",
                mode
            );

            // 2. Assert first_visible and last_visible match linear scan
            assert_eq!(
                queue.first_visible(mode),
                linear_visible.first().copied(),
                "first_visible mismatch for {:?}",
                mode
            );
            assert_eq!(
                queue.last_visible(mode),
                linear_visible.last().copied(),
                "last_visible mismatch for {:?}",
                mode
            );

            // 3. Assert visible_position matches linear scan
            for (pos, &idx) in linear_visible.iter().enumerate() {
                assert_eq!(
                    queue.visible_position(idx, mode),
                    Some(pos),
                    "visible_position mismatch for idx {} in {:?}",
                    idx,
                    mode
                );
            }

            // Non-visible indices return None
            for idx in 0..total {
                if !linear_visible.contains(&idx) {
                    assert_eq!(
                        queue.visible_position(idx, mode),
                        None,
                        "non-visible idx {} returned Some in {:?}",
                        idx,
                        mode
                    );
                }
            }

            // 4. Assert next_visible matches linear scan with wrapping
            if !linear_visible.is_empty() {
                for from_idx in 0..total {
                    let expected_next = linear_visible
                        .iter()
                        .copied()
                        .find(|&idx| idx > from_idx)
                        .or_else(|| linear_visible.first().copied());
                    assert_eq!(
                        queue.next_visible(from_idx, mode),
                        expected_next,
                        "next_visible mismatch from {} in {:?}",
                        from_idx,
                        mode
                    );

                    // 5. Assert prev_visible matches linear scan with wrapping
                    let expected_prev = linear_visible
                        .iter()
                        .copied()
                        .rfind(|&idx| idx < from_idx)
                        .or_else(|| linear_visible.last().copied());
                    assert_eq!(
                        queue.prev_visible(from_idx, mode),
                        expected_prev,
                        "prev_visible mismatch from {} in {:?}",
                        from_idx,
                        mode
                    );

                    // 6. Assert find_closest_visible matches linear scan
                    let expected_closest = if linear_visible.contains(&from_idx) {
                        Some(from_idx)
                    } else {
                        expected_next
                    };
                    assert_eq!(
                        queue.find_closest_visible(from_idx, mode),
                        expected_closest,
                        "find_closest_visible mismatch from {} in {:?}",
                        from_idx,
                        mode
                    );
                }
            } else {
                for from_idx in 0..total {
                    assert_eq!(queue.next_visible(from_idx, mode), None);
                    assert_eq!(queue.prev_visible(from_idx, mode), None);
                    assert_eq!(queue.find_closest_visible(from_idx, mode), None);
                }
            }
        }
    }
}
