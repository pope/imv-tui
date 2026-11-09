//! Background channel communication, image loading, and cache prefetching for `App`.

use crate::app::cache::{CachedThumbnail, VisibleWindow};
use crate::app::palette::PaletteMode;
use crate::app::{App, InitialLoadState, LoadPhase, ThumbnailState, ThumbnailTelemetry};
use crate::imaging::{LoaderMode, LoaderRequest, PanOffset, Rotation, ZoomFactor};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

impl App {
    /// Returns the sliding prefetch window of up to 5 visible images as a stack struct.
    #[inline]
    pub fn get_sliding_window(&self) -> VisibleWindow {
        VisibleWindow::compute(&self.queue, self.view_mode)
    }

    /// Returns the sliding prefetch window indices as a heap-allocated vector.
    #[cfg(test)]
    pub fn get_sliding_window_indices(&self) -> Vec<usize> {
        self.get_sliding_window().as_slice().to_vec()
    }

    /// Dispatches background prefetch requests for neighboring visible images.
    pub fn trigger_prefetch(&mut self) {
        if self.queue.len() <= 1 {
            return;
        }

        let window = self.get_sliding_window();
        if window.is_empty() {
            return;
        }

        self.prefetch_cache.retain_window(&window);
        for &idx in window.iter() {
            if idx == self.queue.current_index {
                continue;
            }
            if !self.prefetch_cache.has_full_image(idx) {
                let source = self.queue.images()[idx].clone();
                let _ = self.loader_tx.send(LoaderRequest {
                    idx,
                    source,
                    mode: LoaderMode::Prefetch,
                    sequence: self.current_sequence,
                });
            }
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

        self.resize_state.complete_in_flight();
        self.load_phase = LoadPhase::Complete;
        self.error_message = None;
        self.screen_clear.defer_until_protocol();
        self.slideshow_last_transition = std::time::Instant::now();

        let adj = self.queue.adjustment(self.queue.current_index);

        // Check if the image is in the prefetch cache
        let cached = self.prefetch_cache.get(self.queue.current_index).cloned();

        let mut loaded_cached_thumb = false;

        if let Some(cached_img) = cached {
            if let Some(ref img) = cached_img.image {
                self.current_sequence += 1;
                self.active_sequence
                    .store(self.current_sequence as usize, Ordering::Release);
                let display_img = if let Some(rotated) = adj.rotate_image(img) {
                    self.img_width = rotated.width();
                    self.img_height = rotated.height();
                    Arc::new(rotated)
                } else {
                    self.img_width = cached_img.width;
                    self.img_height = cached_img.height;
                    img.clone()
                };
                self.original_image = Some(display_img);

                match cached_img.thumbnail {
                    ThumbnailState::Loaded(cached_thumb) => {
                        let rotated_thumb = adj
                            .rotate_image(&cached_thumb.image)
                            .map(Arc::new)
                            .unwrap_or(cached_thumb.image);
                        self.stats.thumbnail = ThumbnailState::Loaded(ThumbnailTelemetry {
                            load_duration: cached_thumb.decode_duration,
                            dimensions: (rotated_thumb.width(), rotated_thumb.height()),
                            source: cached_thumb.source,
                        });
                        self.thumbnail_image = Some(rotated_thumb);
                    }
                    ThumbnailState::Skipped => {
                        self.thumbnail_image = None;
                        self.stats.thumbnail = ThumbnailState::Skipped;
                    }
                    ThumbnailState::Absent => {
                        self.thumbnail_image = None;
                        self.stats.thumbnail = ThumbnailState::Absent;
                    }
                }
                self.show_thumbnail_only = false;
                self.load_phase = LoadPhase::Complete;

                self.zoom_factor = ZoomFactor::DEFAULT;
                self.pan_offset = PanOffset::ZERO;
                self.initial_load = InitialLoadState::Idle;
                self.resize_state.mark_dirty_with_init_zoom();

                // Set stats for cache hit
                self.stats.load_duration = Some(cached_img.decode_duration);
                self.stats.is_prefetch_cache_hit = true;
                self.stats.disk_size = cached_img.disk_size;
                self.stats.format = cached_img.format;
                self.stats.raw_dimensions = cached_img.raw_dimensions;

                let is_fast_scroll = self.scroll_cadence.record_navigation(Instant::now());

                if !is_fast_scroll {
                    self.trigger_prefetch();
                }
                return;
            } else if let ThumbnailState::Loaded(cached_thumb) = cached_img.thumbnail {
                // If we only have a thumbnail, display it immediately as a fast placeholder,
                // but continue loading the full resolution image in the background.
                let rotated_thumb = adj
                    .rotate_image(&cached_thumb.image)
                    .map(Arc::new)
                    .unwrap_or(cached_thumb.image);
                let (tw, th) = (rotated_thumb.width(), rotated_thumb.height());
                self.thumbnail_image = Some(rotated_thumb.clone());
                self.original_image = Some(rotated_thumb);

                let (orig_w, orig_h) =
                    if adj.rotation == Rotation::D90 || adj.rotation == Rotation::D270 {
                        (cached_img.height, cached_img.width)
                    } else {
                        (cached_img.width, cached_img.height)
                    };
                self.img_width = orig_w;
                self.img_height = orig_h;

                self.show_thumbnail_only = true;
                self.resize_state.mark_dirty_with_init_zoom();
                loaded_cached_thumb = true;
                self.stats.is_prefetch_cache_hit = true;
                self.stats.thumbnail = ThumbnailState::Loaded(ThumbnailTelemetry {
                    load_duration: cached_thumb.decode_duration,
                    dimensions: (tw, th),
                    source: cached_thumb.source,
                });
                self.stats.raw_dimensions = cached_img.raw_dimensions;
            }
        }

        // Cache miss (or thumbnail-only hit): load as normal via background loader worker
        if !loaded_cached_thumb {
            self.original_image = None;
            self.thumbnail_image = None;
            self.show_thumbnail_only = false;
            self.stats.is_prefetch_cache_hit = false;
            self.stats.thumbnail = ThumbnailState::Absent;
            self.stats.raw_dimensions = None;
        }
        self.stats.load_duration = None;
        self.image_protocol = None;
        self.initial_load = InitialLoadState::start();
        self.current_sequence += 1;
        self.active_sequence
            .store(self.current_sequence as usize, Ordering::Release);

        self.zoom_factor = ZoomFactor::DEFAULT;
        self.pan_offset = PanOffset::ZERO;
        if loaded_cached_thumb {
            self.resize_state.mark_dirty_with_init_zoom();
        } else {
            self.resize_state.reset_idle();
        }

        let is_fast_scroll = self.scroll_cadence.record_navigation(Instant::now());

        let thumb_only = is_fast_scroll && !self.disable_thumbnail;
        if loaded_cached_thumb && thumb_only {
            self.load_phase = LoadPhase::FullDeferred;
        } else {
            let mode = if thumb_only {
                self.load_phase = LoadPhase::ThumbnailInFlightDeferred;
                LoaderMode::ThumbnailOnly
            } else if loaded_cached_thumb {
                self.load_phase = LoadPhase::FullInFlight;
                LoaderMode::FullOnly
            } else {
                self.load_phase = LoadPhase::FullInFlight;
                LoaderMode::ThumbnailAndFull
            };
            let source = self.queue.images()[self.queue.current_index].clone();
            let _ = self.loader_tx.send(LoaderRequest {
                idx: self.queue.current_index,
                source,
                mode,
                sequence: self.current_sequence,
            });
        }

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
                    let shared_img = decoded.image;
                    let w = decoded.width;
                    let h = decoded.height;
                    let format = decoded.format;
                    let disk_size = decoded.disk_size;
                    let window = self.get_sliding_window();
                    if window.contains(resp.idx) {
                        if let Some(thumb_src) = resp.thumbnail_source {
                            self.prefetch_cache.upsert_thumbnail(
                                resp.idx,
                                CachedThumbnail {
                                    image: shared_img.clone(),
                                    source: thumb_src,
                                    decode_duration: resp.decode_duration,
                                },
                                w,
                                h,
                                format,
                                disk_size,
                                decoded.raw_dimensions,
                            );
                        } else {
                            self.prefetch_cache.upsert_full(
                                resp.idx,
                                shared_img.clone(),
                                resp.decode_duration,
                                w,
                                h,
                                format,
                                disk_size,
                                decoded.raw_dimensions,
                                resp.is_prefetch,
                            );
                        }
                    }

                    if !resp.is_prefetch && resp.idx == self.queue.current_index {
                        let adj = self.queue.adjustment(resp.idx);
                        let rotated_img = adj
                            .rotate_image(&shared_img)
                            .map(Arc::new)
                            .unwrap_or(shared_img);

                        if let Some(thumb_src) = resp.thumbnail_source {
                            if self.load_phase == LoadPhase::Complete
                                && !self.show_thumbnail_only
                                && self.original_image.is_some()
                            {
                                self.stats.thumbnail = ThumbnailState::Loaded(ThumbnailTelemetry {
                                    load_duration: resp.decode_duration,
                                    dimensions: (rotated_img.width(), rotated_img.height()),
                                    source: thumb_src,
                                });
                                self.thumbnail_image = Some(rotated_img);
                            } else {
                                if self.load_phase == LoadPhase::ThumbnailInFlightDeferred {
                                    self.load_phase = LoadPhase::FullDeferred;
                                }
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
                                self.show_thumbnail_only = true;
                                self.error_message = None;
                                self.resize_state.mark_dirty_with_init_zoom();

                                self.stats.thumbnail = ThumbnailState::Loaded(ThumbnailTelemetry {
                                    load_duration: resp.decode_duration,
                                    dimensions: (thumb_w, thumb_h),
                                    source: thumb_src,
                                });
                                self.stats.is_prefetch_cache_hit = false;
                                self.stats.disk_size = disk_size;
                                self.stats.format = format;
                                self.stats.raw_dimensions = decoded.raw_dimensions;
                            }
                        } else {
                            self.img_width = rotated_img.width();
                            self.img_height = rotated_img.height();
                            let had_thumbnail_only = self.show_thumbnail_only;
                            self.original_image = Some(rotated_img);
                            self.show_thumbnail_only = false;
                            self.load_phase = LoadPhase::Complete;
                            self.error_message = None;
                            self.initial_load = InitialLoadState::Idle;
                            if had_thumbnail_only {
                                self.resize_state.mark_dirty();
                            } else {
                                self.resize_state.mark_dirty_with_init_zoom();
                            }

                            self.stats.load_duration = Some(resp.decode_duration);
                            self.stats.is_prefetch_cache_hit = false;
                            if self.stats.thumbnail.is_skipped() {
                                self.stats.thumbnail = ThumbnailState::Absent;
                            }
                            self.stats.disk_size = disk_size;
                            self.stats.format = format;
                            self.stats.raw_dimensions = decoded.raw_dimensions;

                            if self.scroll_cadence.is_settled() {
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
            self.resize_state.complete_in_flight();
            self.initial_load = InitialLoadState::Idle;
            self.screen_clear
                .on_protocol_received(self.palette_mode != PaletteMode::Closed);
            self.stats.process_duration = resp.process_duration;
            self.stats.protocol_duration = resp.protocol_duration;
            self.stats.protocol_width = resp.target_width;
            self.stats.protocol_height = resp.target_height;
        }
        received
    }

    /// Periodic background tick called from the main event loop.
    ///
    /// Promotes deferred thumbnail placeholders to full-resolution images and resumes
    /// suspended background prefetching once fast scrolling ceases and the elapsed time
    /// exceeds `FAST_SCROLL_THRESHOLD`.
    pub fn tick(&mut self) {
        if self.scroll_cadence.poll_settled() {
            if self.load_phase.needs_promotion() && !self.queue.is_empty() {
                self.load_phase = LoadPhase::FullInFlight;
                let mode = if self.thumbnail_image.is_some() {
                    LoaderMode::FullOnly
                } else {
                    LoaderMode::ThumbnailAndFull
                };
                let source = self.queue.images()[self.queue.current_index].clone();
                let _ = self.loader_tx.send(LoaderRequest {
                    idx: self.queue.current_index,
                    source,
                    mode,
                    sequence: self.current_sequence,
                });
            }
            self.trigger_prefetch();
        }
    }

    /// Returns true if any foreground load, full-resolution decode, resize, or deferred fast-scroll
    /// promotion is currently in flight, signaling the event loop to poll at 1ms instead of 33ms.
    pub fn has_pending_work(&self) -> bool {
        if self.error_message.is_some() || self.queue.is_empty() {
            return false;
        }
        self.is_loading()
            || self.needs_update()
            || self.resize_state.is_in_flight()
            || self.is_prefetch_suspended()
            || self.load_phase.is_pending()
    }

    /// Returns true if a renderable protocol is ready (or no load is active) and no layout/resize work is pending.
    #[inline]
    pub fn is_frame_ready(&self) -> bool {
        (!self.initial_load.is_active() || self.image_protocol.is_some())
            && !self.resize_state.is_busy()
    }

    /// Pumps background worker channels and layout updates for up to `budget` while waiting
    /// for an initial thumbnail/image protocol or in-flight resize to complete on Frame 1.
    pub fn wait_for_frame_ready(
        &mut self,
        term_width: u16,
        term_height: u16,
        budget: std::time::Duration,
    ) -> bool {
        let should_wait = (self.initial_load.take_frame_ready_pending() && !self.is_frame_ready())
            || self.resize_state.is_busy();
        if !should_wait {
            return false;
        }
        let start = Instant::now();
        let mut updated = false;
        loop {
            if self.update_channels() {
                updated = true;
            }
            self.update_layout(term_width, term_height);
            if self.is_frame_ready() {
                break;
            }
            if start.elapsed() >= budget {
                break;
            }
            std::thread::sleep(std::time::Duration::from_micros(200));
        }
        updated
    }

    /// Updates the slideshow transition timer by `delta` and advances to the next image when due.
    /// Returns `true` if a slideshow transition occurred.
    pub fn tick_slideshow(&mut self, delta: std::time::Duration) -> bool {
        if self.slideshow_state.is_active()
            && (self.palette_mode != PaletteMode::Closed || self.slideshow_state.is_paused())
        {
            self.slideshow_last_transition += delta;
        }

        if self.slideshow_state.is_playing()
            && !self.is_loading()
            && self.palette_mode == PaletteMode::Closed
            && self.slideshow_last_transition.elapsed()
                >= std::time::Duration::from_secs(self.slideshow_state.seconds() as u64)
        {
            self.next_image();
            self.slideshow_last_transition = Instant::now();
            return true;
        }
        false
    }

    #[cfg(test)]
    /// Drains all incoming worker responses from loader and resizer channels,
    /// updating state and triggering protocol resize requests if widget size is non-zero.
    pub fn check_background_responses(&mut self) -> bool {
        let updated = self.update_channels();
        if self.resize_state.is_dirty()
            && self.last_widget_size.0 > 0
            && self.last_widget_size.1 > 0
        {
            let (w, h) = self.last_widget_size;
            self.update_protocol(w, h);
        }
        updated
    }
}
