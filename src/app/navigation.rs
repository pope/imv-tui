//! Navigation, filtering, and classification management for `App`.

use crate::app::classifications::{Classification, ViewMode, export_to_file, import_from_file};
use crate::app::{App, ScanConfig};
use crate::imaging::{ImageSource, list_cbz_pages, scan_directory};
use std::sync::atomic::Ordering;

impl App {
    /// Returns a slice of image classifications in the active queue.
    #[inline]
    pub fn classifications(&self) -> &[Classification] {
        self.queue.classifications()
    }

    /// Returns a slice of image adjustments in the active queue.
    #[inline]
    #[cfg(test)]
    pub fn adjustments(&self) -> &[crate::app::adjustments::ImageAdjustments] {
        self.queue.adjustments()
    }

    /// Returns a mutable slice of image adjustments in the active queue.
    #[inline]
    #[cfg(test)]
    pub fn adjustments_mut(&mut self) -> &mut [crate::app::adjustments::ImageAdjustments] {
        self.queue.adjustments_mut()
    }

    /// Returns true if the image at `index` is visible under the active view mode.
    #[inline]
    pub fn is_visible(&self, index: usize) -> bool {
        self.queue.is_visible(index, self.view_mode)
    }

    /// Returns the total number of visible images under the current view filter in O(1).
    #[inline]
    pub fn get_visible_count(&self) -> usize {
        self.queue.visible_count(self.view_mode)
    }

    /// Returns the 0-based position of the current image among all visible images in O(N/64).
    #[inline]
    pub fn get_visible_position(&self) -> Option<usize> {
        self.queue
            .visible_position(self.queue.current_index, self.view_mode)
    }

    /// Finds the closest visible index starting at start_idx, checking current index, then scanning forward.
    pub fn find_closest_visible_index(&self, start_idx: usize) -> Option<usize> {
        self.queue.find_closest_visible(start_idx, self.view_mode)
    }

    /// Updates current_index to the closest visible image, or triggers an empty state if none are visible.
    pub fn adjust_current_index_for_visibility(&mut self) {
        if let Some(idx) = self.find_closest_visible_index(self.queue.current_index) {
            let was_empty =
                self.error_message.as_deref() == Some("No images match the current view filter");
            if idx != self.queue.current_index || was_empty {
                self.queue.current_index = idx;
                self.start_load_image();
            } else {
                self.trigger_prefetch();
            }
        } else {
            self.current_sequence += 1;
            self.active_sequence
                .store(self.current_sequence as usize, Ordering::Release);
            self.set_error_state("No images match the current view filter");
            self.needs_update = true;
            self.needs_clear = true;
        }
    }

    /// Flags the current image as a Pick.
    pub fn mark_pick(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        let idx = self.queue.current_index;
        self.queue.set_classification(idx, Classification::Pick);
        if !self.is_visible(idx) {
            self.adjust_current_index_for_visibility();
        }
    }

    /// Flags the current image as a Reject.
    pub fn mark_reject(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        let idx = self.queue.current_index;
        self.queue.set_classification(idx, Classification::Reject);
        if !self.is_visible(idx) {
            self.adjust_current_index_for_visibility();
        }
    }

    /// Clears any Pick/Reject flags from the current image.
    pub fn unflag_image(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        let idx = self.queue.current_index;
        self.queue
            .set_classification(idx, Classification::Unflagged);
        if !self.is_visible(idx) {
            self.adjust_current_index_for_visibility();
        }
    }

    /// Cycles through the available ViewModes (Default -> Unflagged -> Picks -> Rejects -> All).
    pub fn cycle_view_mode(&mut self) {
        self.view_mode = match self.view_mode {
            ViewMode::Default => ViewMode::Unflagged,
            ViewMode::Unflagged => ViewMode::Picks,
            ViewMode::Picks => ViewMode::Rejects,
            ViewMode::Rejects => ViewMode::All,
            ViewMode::All => ViewMode::Default,
        };
        self.adjust_current_index_for_visibility();
    }

    /// Sets the active ViewMode filter directly.
    pub fn set_view_mode(&mut self, mode: ViewMode) {
        self.view_mode = mode;
        self.adjust_current_index_for_visibility();
    }

    /// Returns the user-facing display label for the active ViewMode.
    pub fn view_mode_name(&self) -> &'static str {
        self.view_mode.name()
    }

    /// Returns the classification state of the currently selected image.
    pub fn current_classification(&self) -> Classification {
        if self.queue.is_empty() {
            return Classification::Unflagged;
        }
        self.queue.classification(self.queue.current_index)
    }

    /// Re-scans the directory or CBZ archive to pick up any added or removed files, preserving existing classifications/adjustments.
    pub fn refresh_files(&mut self) -> Result<(), String> {
        let new_images = match &self.scan_config {
            ScanConfig::Piped => {
                return Err("Refresh is not supported for piped stdin inputs".to_string());
            }
            ScanConfig::Cbz(zip_path) => {
                let pages = list_cbz_pages(zip_path)?;
                pages
                    .into_iter()
                    .map(|page| ImageSource::Cbz {
                        zip_path: zip_path.clone(),
                        file_in_zip: page,
                    })
                    .collect::<Vec<_>>()
            }
            ScanConfig::Directory {
                path,
                check_magic,
                recursive,
            } => {
                let (paths, _) = scan_directory(path, *check_magic, *recursive)?;
                paths
                    .into_iter()
                    .map(ImageSource::Local)
                    .collect::<Vec<_>>()
            }
        };

        if new_images.is_empty() {
            return Err("No supported images found".to_string());
        }

        // Clear prefetch cache immediately since index associations have changed
        self.prefetch_cache.lock().unwrap().clear();

        // Atomically rescan and preserve existing classifications and adjustments
        self.queue
            .rescan_preserving_state(new_images, &mut self.unmapped_classifications)?;

        // Reset search/fuzzy matching filtered files
        if self.palette_mode == crate::app::PaletteMode::File {
            self.update_palette_filter();
        } else {
            self.filtered_files.clear();
            if self.filtered_files.capacity() > crate::app::palette::MAX_IDLE_PALETTE_CAPACITY {
                self.filtered_files
                    .shrink_to(crate::app::palette::MAX_IDLE_PALETTE_CAPACITY);
            }
        }

        // Adjust selected index for visibility in case files are filtered out
        self.adjust_current_index_for_visibility();

        // Reload current image to apply adjustments immediately
        self.start_load_image();

        self.needs_update = true;
        self.needs_clear = true;

        Ok(())
    }

    /// Imports image classification states and adjustments from a text file or a JSON manifest.
    pub fn import_classifications(&mut self, import_path: &std::path::Path) -> Result<(), String> {
        let mut imported = import_from_file(import_path)?;
        let cwd = std::env::current_dir().ok();

        // Apply imported states to existing files
        let mut updates = Vec::new();
        for (idx, img) in self.queue.images().iter().enumerate() {
            let ident = img.identifier_with_cwd(cwd.as_deref());
            if let Some((class, adj)) = imported.remove(&ident) {
                updates.push((idx, class, adj));
            }
            self.unmapped_classifications.remove(&ident);
        }

        for (idx, class, adj) in updates {
            self.queue.set_classification(idx, class);
            if let Some(slot) = self.queue.adjustments_mut().get_mut(idx) {
                *slot = adj;
            }
        }

        // Store the remaining unmapped entries
        self.unmapped_classifications.extend(imported);

        // Adjust selected index for visibility in case files are filtered out
        self.adjust_current_index_for_visibility();

        // Reload current image to apply adjustments immediately
        self.start_load_image();

        Ok(())
    }

    /// Exports image classification states and adjustments to a text file or a JSON manifest.
    pub fn export_classifications(&self, export_path: &std::path::Path) -> Result<(), String> {
        export_to_file(
            export_path,
            self.queue.images(),
            self.queue.classifications(),
            self.queue.adjustments(),
            &self.unmapped_classifications,
        )
    }

    /// Returns the display path of the currently selected image, or an informative string if empty.
    pub fn current_relative_path(&self) -> &str {
        if self.queue.is_empty() || self.get_visible_count() == 0 {
            return "No file loaded";
        }
        self.queue
            .relative_paths()
            .get(self.queue.current_index)
            .map(|s| s.as_str())
            .unwrap_or("Unknown file")
    }

    /// Navigates to the next visible image in the active queue.
    pub fn next_image(&mut self) {
        let total = self.queue.len();
        if total <= 1 {
            return;
        }
        let start = self.queue.current_index;
        if let Some(next_idx) = self.queue.next_visible(start, self.view_mode)
            && next_idx != start
        {
            self.queue.current_index = next_idx;
            self.start_load_image();
        }
    }

    /// Navigates to the previous visible image in the active queue.
    pub fn prev_image(&mut self) {
        let total = self.queue.len();
        if total <= 1 {
            return;
        }
        let start = self.queue.current_index;
        if let Some(prev_idx) = self.queue.prev_visible(start, self.view_mode)
            && prev_idx != start
        {
            self.queue.current_index = prev_idx;
            self.start_load_image();
        }
    }
}
