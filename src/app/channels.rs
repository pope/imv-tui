//! Background channel communication, image loading, and cache prefetching for `App`.

use crate::app::cache::{CachedImage, VisibleWindow};
use crate::app::palette::PaletteMode;
use crate::app::{App, FAST_SCROLL_THRESHOLD};
use crate::imaging::{LoaderRequest, PanOffset, Rotation, ZoomFactor};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

impl App {
    /// Returns the sliding prefetch window of up to 5 visible images as a stack struct.
    #[inline]
    pub(crate) fn get_sliding_window(&self) -> VisibleWindow {
        VisibleWindow::compute(&self.queue, self.view_mode)
    }

    /// Returns the sliding prefetch window indices as a heap-allocated vector.
    #[cfg(test)]
    pub(crate) fn get_sliding_window_indices(&self) -> Vec<usize> {
        self.get_sliding_window().as_slice().to_vec()
    }

    /// Dispatches background prefetch requests for neighboring visible images.
    pub fn trigger_prefetch(&self) {
        if self.queue.len() <= 1 {
            return;
        }

        let window = self.get_sliding_window();
        if window.is_empty() {
            return;
        }
        let mut to_prefetch = Vec::new();

        {
            let mut cache = self.prefetch_cache.lock().unwrap();
            cache.retain(|idx, _| window.contains(*idx));
            for &idx in window.iter() {
                if idx == self.queue.current_index {
                    continue;
                }
                if !cache.contains_key(&idx) {
                    to_prefetch.push(idx);
                }
            }
        }

        for idx in to_prefetch {
            let source = self.queue.images()[idx].clone();
            let _ = self.loader_tx.send(LoaderRequest {
                idx,
                source,
                is_prefetch: true,
                sequence: self.current_sequence,
            });
        }
    }

    /// Initiates loading the currently selected image, checking prefetch cache first.
    pub fn start_load_image(&mut self) {
        if self.queue.is_empty() {
            self.current_sequence += 1;
            self.active_sequence
                .store(self.current_sequence as usize, Ordering::Release);
            self.set_error_state("No supported images found");
            return;
        }

        self.error_message = None;
        self.clear_on_protocol_receive = true;
        self.slideshow_last_transition = std::time::Instant::now();

        let adj = self.queue.adjustment(self.queue.current_index);

        // Check if the image is in the prefetch cache
        let cached = {
            let cache = self.prefetch_cache.lock().unwrap();
            cache.get(&self.queue.current_index).cloned()
        };

        if let Some(cached_img) = cached {
            if let Some(ref img) = cached_img.image {
                self.current_sequence += 1;
                self.active_sequence
                    .store(self.current_sequence as usize, Ordering::Release);
                self.original_image = Some(img.clone()).map(|img| {
                    if let Some(rotated) = adj.rotate_image(&img) {
                        self.img_width = rotated.width();
                        self.img_height = rotated.height();
                        Arc::new(rotated)
                    } else {
                        self.img_width = cached_img.width;
                        self.img_height = cached_img.height;
                        img
                    }
                });

                self.thumbnail_image = cached_img
                    .thumbnail
                    .map(|thumb| adj.rotate_image(&thumb).map(Arc::new).unwrap_or(thumb));
                self.show_thumbnail_only = false;

                if let Some(ref thumb) = self.thumbnail_image {
                    self.stats.thumbnail_load_duration = Some(cached_img.thumbnail_decode_duration);
                    self.stats.thumbnail_dimensions = Some((thumb.width(), thumb.height()));
                } else {
                    self.stats.thumbnail_load_duration = None;
                    self.stats.thumbnail_dimensions = None;
                }

                self.zoom_factor = ZoomFactor::DEFAULT;
                self.pan_offset = PanOffset::ZERO;
                self.is_loading = false;
                self.needs_update = true;
                self.zoom_needs_initialization = true;

                // Set stats for cache hit
                self.stats.load_duration = cached_img.decode_duration;
                self.stats.is_prefetch_cache_hit = true;
                self.stats.disk_size = cached_img.disk_size;
                self.stats.format = cached_img.format;
                self.stats.raw_width = cached_img.raw_width;
                self.stats.raw_height = cached_img.raw_height;

                let now = Instant::now();
                let is_fast_scroll = self
                    .last_nav_time
                    .is_some_and(|prev| now.duration_since(prev) < FAST_SCROLL_THRESHOLD);
                self.last_nav_time = Some(now);
                self.prefetch_suspended = is_fast_scroll;

                if !is_fast_scroll {
                    self.trigger_prefetch();
                }
                return;
            } else if let Some(ref thumb) = cached_img.thumbnail {
                // If we only have a thumbnail, display it immediately as a fast placeholder,
                // but continue loading the full resolution image in the background.
                self.thumbnail_image = Some(thumb.clone())
                    .map(|thumb| adj.rotate_image(&thumb).map(Arc::new).unwrap_or(thumb));
                self.original_image = self.thumbnail_image.clone();

                let (orig_w, orig_h) =
                    if adj.rotation == Rotation::D90 || adj.rotation == Rotation::D270 {
                        (cached_img.height, cached_img.width)
                    } else {
                        (cached_img.width, cached_img.height)
                    };
                self.img_width = orig_w;
                self.img_height = orig_h;

                self.show_thumbnail_only = true;
                self.stats.thumbnail_load_duration = Some(cached_img.thumbnail_decode_duration);
                self.stats.thumbnail_dimensions = Some((cached_img.width, cached_img.height));
                self.stats.raw_width = cached_img.raw_width;
                self.stats.raw_height = cached_img.raw_height;
            }
        }

        // Cache miss (or thumbnail-only hit): load as normal via background loader worker
        if self.thumbnail_image.is_none() {
            self.original_image = None;
            self.thumbnail_image = None;
            self.show_thumbnail_only = false;
            self.stats.thumbnail_load_duration = None;
            self.stats.thumbnail_dimensions = None;
            self.stats.raw_width = None;
            self.stats.raw_height = None;
        }
        self.image_protocol = None;
        self.is_loading = true;
        self.loading_start_time = Some(Instant::now());
        self.current_sequence += 1;
        self.active_sequence
            .store(self.current_sequence as usize, Ordering::Release);

        self.zoom_factor = ZoomFactor::DEFAULT;
        self.pan_offset = PanOffset::ZERO;
        self.zoom_needs_initialization = true;

        let source = self.queue.images()[self.queue.current_index].clone();
        let _ = self.loader_tx.send(LoaderRequest {
            idx: self.queue.current_index,
            source,
            is_prefetch: false,
            sequence: self.current_sequence,
        });

        let now = Instant::now();
        let is_fast_scroll = self
            .last_nav_time
            .is_some_and(|prev| now.duration_since(prev) < FAST_SCROLL_THRESHOLD);
        self.last_nav_time = Some(now);
        self.prefetch_suspended = is_fast_scroll;

        if !is_fast_scroll {
            self.trigger_prefetch();
        }
    }

    /// Receives responses from loader and resizer worker channels, updating application state.
    pub fn update_channels(&mut self) -> bool {
        let mut received = false;
        while let Ok(resp) = self.response_rx.try_recv() {
            received = true;
            if resp.sequence < self.current_sequence {
                continue;
            }

            match resp.result {
                Ok(decoded) => {
                    let img = decoded.image;
                    let w = decoded.width;
                    let h = decoded.height;
                    let format = decoded.format;
                    let disk_size = decoded.disk_size;
                    let shared_img = Arc::new(img);
                    let window = self.get_sliding_window();
                    if window.contains(resp.idx) {
                        let mut cache = self.prefetch_cache.lock().unwrap();
                        if resp.is_thumbnail {
                            if let Some(cached) = cache.get_mut(&resp.idx) {
                                cached.thumbnail = Some(shared_img.clone());
                                cached.thumbnail_decode_duration = resp.decode_duration;
                                cached.raw_width = decoded.raw_width;
                                cached.raw_height = decoded.raw_height;
                            } else {
                                cache.insert(
                                    resp.idx,
                                    CachedImage {
                                        image: None,
                                        thumbnail: Some(shared_img.clone()),
                                        width: w,
                                        height: h,
                                        format,
                                        decode_duration: std::time::Duration::ZERO,
                                        thumbnail_decode_duration: resp.decode_duration,
                                        disk_size,
                                        raw_width: decoded.raw_width,
                                        raw_height: decoded.raw_height,
                                    },
                                );
                            }
                        } else {
                            if let Some(cached) = cache.get_mut(&resp.idx) {
                                cached.image = Some(shared_img.clone());
                                cached.width = w;
                                cached.height = h;
                                cached.format = format;
                                cached.decode_duration = resp.decode_duration;
                                cached.disk_size = disk_size;
                                cached.raw_width = decoded.raw_width;
                                cached.raw_height = decoded.raw_height;
                            } else {
                                cache.insert(
                                    resp.idx,
                                    CachedImage {
                                        image: Some(shared_img.clone()),
                                        thumbnail: None,
                                        width: w,
                                        height: h,
                                        format,
                                        decode_duration: resp.decode_duration,
                                        thumbnail_decode_duration: std::time::Duration::ZERO,
                                        disk_size,
                                        raw_width: decoded.raw_width,
                                        raw_height: decoded.raw_height,
                                    },
                                );
                            }
                        }
                    }

                    if !resp.is_prefetch && resp.idx == self.queue.current_index {
                        let adj = self.queue.adjustment(resp.idx);
                        let rotated_img = adj
                            .rotate_image(&shared_img)
                            .map(Arc::new)
                            .unwrap_or(shared_img);

                        if resp.is_thumbnail {
                            let (orig_w, orig_h) = if adj.rotation == Rotation::D90
                                || adj.rotation == Rotation::D270
                            {
                                (h, w)
                            } else {
                                (w, h)
                            };
                            self.img_width = orig_w;
                            self.img_height = orig_h;
                            let thumb_w = rotated_img.width();
                            let thumb_h = rotated_img.height();
                            self.original_image = Some(rotated_img.clone());
                            self.thumbnail_image = Some(rotated_img);
                            self.error_message = None;
                            self.needs_update = true;

                            self.stats.thumbnail_load_duration = Some(resp.decode_duration);
                            self.stats.thumbnail_dimensions = Some((thumb_w, thumb_h));
                            self.stats.is_prefetch_cache_hit = false;
                            self.stats.disk_size = disk_size;
                            self.stats.format = format;
                            self.stats.raw_width = decoded.raw_width;
                            self.stats.raw_height = decoded.raw_height;
                        } else {
                            self.img_width = rotated_img.width();
                            self.img_height = rotated_img.height();
                            self.original_image = Some(rotated_img);
                            self.show_thumbnail_only = false;
                            self.error_message = None;
                            self.is_loading = false;
                            self.needs_update = true;

                            self.stats.load_duration = resp.decode_duration;
                            self.stats.is_prefetch_cache_hit = false;
                            self.stats.disk_size = disk_size;
                            self.stats.format = format;
                            self.stats.raw_width = decoded.raw_width;
                            self.stats.raw_height = decoded.raw_height;

                            if self
                                .last_nav_time
                                .is_none_or(|t| t.elapsed() >= FAST_SCROLL_THRESHOLD)
                            {
                                self.trigger_prefetch();
                            }
                        }
                    }
                }
                Err(err) => {
                    if !resp.is_prefetch && resp.idx == self.queue.current_index {
                        self.set_error_state(err);
                    }
                }
            }
        }

        let mut latest_protocol = None;
        while let Ok(resp) = self.protocol_rx.try_recv() {
            if resp.sequence == self.current_sequence {
                latest_protocol = Some(resp);
            }
        }

        if let Some(resp) = latest_protocol {
            received = true;
            self.image_protocol = Some(resp.protocol);
            self.rendered_size_cells = resp.rendered_cells;
            self.is_loading = false;
            if self.clear_on_protocol_receive {
                self.clear_on_protocol_receive = false;
                self.needs_clear = true;
            }
            if self.palette_mode != PaletteMode::Closed {
                self.needs_clear = true;
            }
            self.stats.process_duration = resp.process_duration;
            self.stats.protocol_duration = resp.protocol_duration;
            self.stats.protocol_width = resp.target_width;
            self.stats.protocol_height = resp.target_height;
        }
        received
    }

    /// Periodic background tick called from the main event loop.
    ///
    /// Resumes suspended background prefetching once fast scrolling ceases
    /// and the elapsed time exceeds `FAST_SCROLL_THRESHOLD`.
    pub(crate) fn tick(&mut self) {
        if self.prefetch_suspended
            && self
                .last_nav_time
                .is_some_and(|t| t.elapsed() >= FAST_SCROLL_THRESHOLD)
        {
            self.prefetch_suspended = false;
            self.trigger_prefetch();
        }
    }

    #[cfg(test)]
    /// Drains all incoming worker responses from loader and resizer channels,
    /// updating state and triggering protocol resize requests if widget size is non-zero.
    pub(crate) fn check_background_responses(&mut self) -> bool {
        let updated = self.update_channels();
        if self.needs_update && self.last_widget_size.0 > 0 && self.last_widget_size.1 > 0 {
            let (w, h) = self.last_widget_size;
            self.needs_update = false;
            self.update_protocol(w, h);
        }
        updated
    }
}
