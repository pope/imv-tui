//! Viewport layout, zooming, panning, adjustments, and rotation for `App`.

use crate::app::palette::PaletteMode;
use crate::app::{App, LoadPhase};
use crate::commands::PanDirection;
use crate::imaging::{
    Brightness, CellDimensions, Contrast, CropBox, ImageIntersection, PanOffset, PixelDimensions,
    ProtocolType, ResizeRequest, Rotation, ScaleMode, ZoomFactor, rotate90_fast, rotate270_fast,
};
use crate::ui::ThumbnailState;
use std::sync::Arc;

/// Signed unit step along a single viewport axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AxisStep {
    /// Negative direction (left or up).
    Neg,
    /// Stationary (no movement along this axis).
    #[default]
    Zero,
    /// Positive direction (right or down).
    Pos,
}

impl AxisStep {
    /// Returns the numeric sign (`-1`, `0`, or `1`) for this axis step.
    pub const fn signum(self) -> i64 {
        match self {
            Self::Neg => -1,
            Self::Zero => 0,
            Self::Pos => 1,
        }
    }
}

/// Two-dimensional directional step vector for viewport panning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PanVector {
    /// Horizontal axis step.
    pub dx: AxisStep,
    /// Vertical axis step.
    pub dy: AxisStep,
}

impl PanVector {
    /// Creates a single-axis [`PanVector`] from a cardinal [`PanDirection`].
    pub const fn from_direction(dir: PanDirection) -> Self {
        match dir {
            PanDirection::Left => Self {
                dx: AxisStep::Neg,
                dy: AxisStep::Zero,
            },
            PanDirection::Right => Self {
                dx: AxisStep::Pos,
                dy: AxisStep::Zero,
            },
            PanDirection::Up => Self {
                dx: AxisStep::Zero,
                dy: AxisStep::Neg,
            },
            PanDirection::Down => Self {
                dx: AxisStep::Zero,
                dy: AxisStep::Pos,
            },
        }
    }

    /// Returns `true` if both horizontal and vertical steps are zero.
    pub const fn is_zero(self) -> bool {
        matches!(self.dx, AxisStep::Zero) && matches!(self.dy, AxisStep::Zero)
    }
}

/// Bitmask tracking which cardinal pan keys are currently held down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HeldPanMask {
    bits: u8,
}

impl HeldPanMask {
    const LEFT: u8 = 1 << 0;
    const RIGHT: u8 = 1 << 1;
    const UP: u8 = 1 << 2;
    const DOWN: u8 = 1 << 3;

    const fn bit(dir: PanDirection) -> u8 {
        match dir {
            PanDirection::Left => Self::LEFT,
            PanDirection::Right => Self::RIGHT,
            PanDirection::Up => Self::UP,
            PanDirection::Down => Self::DOWN,
        }
    }

    /// Marks `dir` as currently held.
    pub fn press(&mut self, dir: PanDirection) {
        self.bits |= Self::bit(dir);
    }

    /// Clears `dir` from the held set.
    pub fn release(&mut self, dir: PanDirection) {
        self.bits &= !Self::bit(dir);
    }

    /// Clears all held directions.
    pub fn clear(&mut self) {
        self.bits = 0;
    }

    /// Computes the combined [`PanVector`] from all currently held directions.
    /// Opposing directions on the same axis (e.g. Left + Right) cancel to [`AxisStep::Zero`].
    pub const fn vector(self) -> PanVector {
        let left = (self.bits & Self::LEFT) != 0;
        let right = (self.bits & Self::RIGHT) != 0;
        let dx = match (left, right) {
            (true, false) => AxisStep::Neg,
            (false, true) => AxisStep::Pos,
            _ => AxisStep::Zero,
        };

        let up = (self.bits & Self::UP) != 0;
        let down = (self.bits & Self::DOWN) != 0;
        let dy = match (up, down) {
            (true, false) => AxisStep::Neg,
            (false, true) => AxisStep::Pos,
            _ => AxisStep::Zero,
        };

        PanVector { dx, dy }
    }
}

/// Type-safe state machine for tracking held pan keys with safe fallback on legacy terminals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HeldPanState {
    /// Terminal does not report key release events; every press is a stateless single-axis step.
    #[default]
    Unsupported,
    /// Terminal supports Kitty keyboard event types; tracks held pan keys for diagonal panning.
    Tracking(HeldPanMask),
}

impl HeldPanState {
    /// Creates a new [`HeldPanState`] based on whether keyboard enhancement is active.
    pub fn new(keyboard_enhanced: bool) -> Self {
        if keyboard_enhanced {
            Self::Tracking(HeldPanMask::default())
        } else {
            Self::Unsupported
        }
    }

    /// Records a press/repeat of `dir` and returns the resulting [`PanVector`].
    pub fn on_press(&mut self, dir: PanDirection) -> PanVector {
        match self {
            Self::Unsupported => PanVector::from_direction(dir),
            Self::Tracking(mask) => {
                mask.press(dir);
                mask.vector()
            }
        }
    }

    /// Records a release of `dir` when tracking is active.
    pub fn on_release(&mut self, dir: PanDirection) {
        if let Self::Tracking(mask) = self {
            mask.release(dir);
        }
    }

    /// Clears all tracked held keys when tracking is active.
    pub fn clear(&mut self) {
        if let Self::Tracking(mask) = self {
            mask.clear();
        }
    }
}

impl App {
    /// Calculates the size of the viewport area, subtracting the infobar height if configured.
    pub fn get_viewport_size(&self, term_width: u16, term_height: u16) -> (u16, u16) {
        let viewport_h = term_height.saturating_sub(self.infobar.height());
        (term_width, viewport_h)
    }

    /// Updates the viewport layout and palette dialog heights based on terminal dimensions.
    pub fn update_layout(&mut self, term_width: u16, term_height: u16) {
        // 1. First, check if the image protocol needs updating based on the main viewport area size
        let (widget_w, widget_h) = self.get_viewport_size(term_width, term_height);
        if self.needs_update() || self.last_widget_size != (widget_w, widget_h) {
            self.last_widget_size = (widget_w, widget_h);
            self.update_protocol(widget_w, widget_h);
        }

        // 2. Then, update the overlay palette height if a palette is open
        if self.palette_mode == PaletteMode::Closed {
            return;
        }

        let new_h = match self.palette_mode {
            PaletteMode::Prompt(_) => 4.min(term_height.saturating_sub(1)),
            PaletteMode::Info => 23.min(term_height.saturating_sub(1)),
            PaletteMode::File | PaletteMode::Command => {
                let total_items = match self.palette_mode {
                    PaletteMode::File => self.get_filtered_files().len(),
                    PaletteMode::Command => self.get_filtered_commands().len(),
                    _ => 0,
                };
                let max_height = (term_height as f64 * 0.5).round() as u16;
                let mut palette_h = (total_items as u16 + 4).max(12);
                palette_h = palette_h.min(max_height);
                palette_h
            }
            _ => 0,
        };

        if self.palette_height != new_h {
            self.palette_height = new_h;
            self.screen_clear.request_now();
        }
    }

    /// Returns the terminal cell dimensions in pixels `(cell_w, cell_h)`, falling back to `(8, 16)`
    /// if the protocol picker reports zero dimensions.
    #[inline]
    pub fn cell_pixel_size(&self) -> (u16, u16) {
        let font_size = self.picker.font_size();
        let cell_w = if font_size.width == 0 {
            8
        } else {
            font_size.width
        };
        let cell_h = if font_size.height == 0 {
            16
        } else {
            font_size.height
        };
        (cell_w, cell_h)
    }

    /// Returns the current viewport widget dimensions in pixels `(widget_w_px, widget_h_px)`.
    #[inline]
    pub fn viewport_pixel_size(&self) -> (f64, f64) {
        let (widget_w_cells, widget_h_cells) = self.last_widget_size;
        let (cell_w, cell_h) = self.cell_pixel_size();
        (
            widget_w_cells as f64 * cell_w as f64,
            widget_h_cells as f64 * cell_h as f64,
        )
    }

    /// Computes the sorted, deduplicated predefined zoom scale levels on the stack.
    pub fn zoom_step_levels(&self, fit_scale: f64) -> ([f64; 6], usize) {
        let (widget_w_px, widget_h_px) = self.viewport_pixel_size();
        let s_w = widget_w_px / self.img_width as f64;
        let s_h = widget_h_px / self.img_height as f64;
        let crop_scale = s_w.max(s_h);

        let mut raw = [fit_scale.min(1.0), fit_scale, crop_scale, 1.0, 2.0, 4.0];
        raw.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        let mut out = [0.0; 6];
        let mut len = 0;
        for lvl in raw {
            if len == 0 || (lvl - out[len - 1]).abs() >= 0.01 {
                out[len] = lvl;
                len += 1;
            }
        }
        (out, len)
    }

    /// Dispatches an image resize request to the background worker pool for the given viewport cell dimensions.
    pub fn update_protocol(&mut self, widget_w: u16, widget_h: u16) {
        let cells = CellDimensions::new(widget_w, widget_h);
        if cells.is_empty() {
            return;
        }

        let (cell_w, cell_h) = self.cell_pixel_size();
        let widget_w_px = cells.cols as f64 * cell_w as f64;
        let widget_h_px = cells.rows as f64 * cell_h as f64;

        let active_img = if self.show_thumbnail_only && self.thumbnail_image.is_some() {
            self.thumbnail_image.as_ref()
        } else {
            self.original_image.as_ref()
        };

        if let Some(img) = active_img {
            let dims = PixelDimensions::new(self.img_width, self.img_height);
            if dims.is_empty() {
                return;
            }
            let w_orig = dims.width as f64;
            let h_orig = dims.height as f64;

            // 1. Calculate fit-to-screen scale 's'
            let s_w = widget_w_px / w_orig;
            let s_h = widget_h_px / h_orig;
            let s = s_w.min(s_h);

            if self.resize_state.take_init_zoom() && s > 0.0 {
                self.zoom_factor = ZoomFactor::new(match self.scale_mode {
                    ScaleMode::None => 1.0 / s,
                    ScaleMode::Shrink => {
                        if s < 1.0 {
                            1.0
                        } else {
                            1.0 / s
                        }
                    }
                    ScaleMode::Full => 1.0,
                    ScaleMode::Crop => s_w.max(s_h) / s,
                });
                self.pan_offset = PanOffset::ZERO;
            }

            // 2. Combined scale is s * zoom_factor
            let scale = s * self.zoom_factor.value();
            self.current_zoom_pct = scale * 100.0;

            // 3. Compute crop window in original image pixels
            let crop_w = (widget_w_px / scale).round() as u32;
            let crop_h = (widget_h_px / scale).round() as u32;
            let crop_w = crop_w.max(1);
            let crop_h = crop_h.max(1);

            // Calculate target rendering size in pixels based on crop and zoom scale
            let target_w = (crop_w as f64 * scale).round() as u32;
            let target_h = (crop_h as f64 * scale).round() as u32;
            let target_w = target_w.max(1);
            let target_h = target_h.max(1);

            // Center of crop window is center of image + pan_offset
            let center_x = (self.img_width as i64 / 2) + self.pan_offset.x;
            let center_y = (self.img_height as i64 / 2) + self.pan_offset.y;

            // Compute top-left of crop box (can be negative if we pan past bounds)
            let crop_x1 = center_x - (crop_w as i64 / 2);
            let crop_y1 = center_y - (crop_h as i64 / 2);
            let crop_x2 = crop_x1 + crop_w as i64;
            let crop_y2 = crop_y1 + crop_h as i64;

            // Intersecting bounds with original image
            let inter_x1 = crop_x1.clamp(0, self.img_width as i64);
            let inter_y1 = crop_y1.clamp(0, self.img_height as i64);
            let inter_x2 = crop_x2.clamp(0, self.img_width as i64);
            let inter_y2 = crop_y2.clamp(0, self.img_height as i64);

            // Calculate exact cell size of the rendered image
            let cells_w = (target_w as f64 / cell_w as f64).round() as u16;
            let cells_h = (target_h as f64 / cell_h as f64).round() as u16;
            let rendered_cells = (cells_w.clamp(1, cells.cols), cells_h.clamp(1, cells.rows));

            let req = ResizeRequest {
                img: Arc::clone(img),
                original_size: (self.img_width, self.img_height),
                scale,
                crop: CropBox::new(crop_x1, crop_y1, crop_x2, crop_y2),
                intersection: ImageIntersection::new(
                    inter_x1 as u32,
                    inter_y1 as u32,
                    inter_x2 as u32,
                    inter_y2 as u32,
                ),
                target_w,
                target_h,
                filter_type: self.filter_type,
                picker: self.picker.clone(),
                brightness: self.current_brightness(),
                contrast: self.current_contrast(),
                rendered_size_cells: rendered_cells,
                sequence: self.current_sequence,
                protocol: self.active_protocol,
            };

            let sent = self.resize_tx.send(req).is_ok();
            self.resize_state.set_in_flight(sent);
        } else {
            self.image_protocol = None;
            self.rendered_size_cells = (0, 0);
            self.resize_state.reset_idle();
        }
    }

    /// Calculates the scaling factor to fit the image inside the current widget viewport bounds.
    pub fn get_fit_scale(&self) -> f64 {
        let dims = PixelDimensions::new(self.img_width, self.img_height);
        if dims.is_empty() {
            return 0.0;
        }
        let (widget_w_cells, widget_h_cells) = self.last_widget_size;
        let cells = CellDimensions::new(widget_w_cells, widget_h_cells);
        if cells.is_empty() {
            return 0.0;
        }
        let (widget_w_px, widget_h_px) = self.viewport_pixel_size();

        let s_w = widget_w_px / self.img_width as f64;
        let s_h = widget_h_px / self.img_height as f64;
        s_w.min(s_h)
    }

    /// Returns the brightness setting for the currently selected image.
    pub fn current_brightness(&self) -> Brightness {
        if self.queue.is_empty() {
            return Brightness::ZERO;
        }
        self.queue.adjustment(self.queue.current_index).brightness
    }

    /// Returns the contrast setting for the currently selected image.
    pub fn current_contrast(&self) -> Contrast {
        if self.queue.is_empty() {
            return Contrast::ZERO;
        }
        self.queue.adjustment(self.queue.current_index).contrast
    }

    /// Returns true if the active terminal graphics protocol requires clearing the screen on updates
    /// (`ProtocolType::Sixel` and `ProtocolType::Iterm2`), so overlays redraw cleanly over updated
    /// inline raster graphics and clear completely on dismissal.
    pub fn should_clear_on_update(&self) -> bool {
        matches!(
            self.picker.protocol_type(),
            ProtocolType::Sixel | ProtocolType::Iterm2
        )
    }

    /// Zooms into the image by a fractional multiplier.
    pub fn zoom_in(&mut self) {
        if self.original_image.is_none() || self.is_loading() {
            return;
        }
        let s = self.get_fit_scale();
        if s > 0.0 {
            self.zoom_factor = ZoomFactor::new((self.zoom_factor.value() * 1.25).min(102.4 / s));
            self.clamp_pan();
            self.resize_state.mark_dirty();
        }
    }

    /// Zooms out of the image by a fractional multiplier.
    pub fn zoom_out(&mut self) {
        if self.original_image.is_none() || self.is_loading() {
            return;
        }
        let s = self.get_fit_scale();
        if s > 0.0 {
            self.zoom_factor = ZoomFactor::new((self.zoom_factor.value() / 1.25).max(0.01 / s));
            self.clamp_pan();
            self.resize_state.mark_dirty();
        }
    }

    /// Jumps forward to the next predefined zoom step.
    pub fn jump_zoom_in(&mut self) {
        if self.original_image.is_none() || self.is_loading() {
            return;
        }
        let fit_scale = self.get_fit_scale();
        if fit_scale <= 0.0 {
            return;
        }

        let (levels, len) = self.zoom_step_levels(fit_scale);
        let levels = &levels[..len];

        let current_scale = fit_scale * self.zoom_factor.value();

        let mut target_scale = None;
        for &lvl in levels {
            if lvl > current_scale + 0.01 {
                target_scale = Some(lvl);
                break;
            }
        }

        if let Some(target) = target_scale {
            self.zoom_factor = ZoomFactor::new(target / fit_scale);
        } else {
            // Double the scale if already past maximum level
            self.zoom_factor = ZoomFactor::new((current_scale * 2.0).min(102.4) / fit_scale);
        }

        self.clamp_pan();
        self.resize_state.mark_dirty();
    }

    /// Jumps backward to the previous predefined zoom step.
    pub fn jump_zoom_out(&mut self) {
        if self.original_image.is_none() || self.is_loading() {
            return;
        }
        let fit_scale = self.get_fit_scale();
        if fit_scale <= 0.0 {
            return;
        }

        let (levels, len) = self.zoom_step_levels(fit_scale);
        let levels = &levels[..len];

        let current_scale = fit_scale * self.zoom_factor.value();

        let mut target_scale = None;
        for &lvl in levels.iter().rev() {
            if lvl < current_scale - 0.01 {
                target_scale = Some(lvl);
                break;
            }
        }

        if let Some(target) = target_scale {
            self.zoom_factor = ZoomFactor::new(target / fit_scale);
        } else {
            // Halve the scale if already below minimum level
            self.zoom_factor = ZoomFactor::new((current_scale / 2.0).max(0.01) / fit_scale);
        }

        self.clamp_pan();
        self.resize_state.mark_dirty();
    }

    /// Sets the zoom factor to actual size (1:1 image pixels to screen pixels).
    pub fn set_actual_size(&mut self) {
        if self.original_image.is_none() || self.is_loading() {
            return;
        }
        let s = self.get_fit_scale();
        if s > 0.0 {
            self.zoom_factor = ZoomFactor::new(1.0 / s);
            self.clamp_pan();
            self.resize_state.mark_dirty();
            self.screen_clear.defer_until_protocol();
        }
    }

    /// Resets the zoom factor and pan offset back to the current scale mode settings.
    pub fn reset_view(&mut self) {
        if self.original_image.is_none() || self.is_loading() {
            return;
        }
        self.apply_scale_mode();
        self.screen_clear.defer_until_protocol();
    }

    /// Resets brightness, contrast, and rotation adjustments back to zero defaults.
    pub fn reset_image(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        let idx = self.queue.current_index;
        if let Some(adj) = self.queue.adjustments_mut().get_mut(idx) {
            adj.rotation = Rotation::D0;
            adj.brightness = Brightness::ZERO;
            adj.contrast = Contrast::ZERO;
        }
        self.start_load_image();
    }

    /// Applies the active ScaleMode to calculate zoom factor and center the pan offset.
    pub fn apply_scale_mode(&mut self) {
        let s = self.get_fit_scale();
        if s > 0.0 {
            self.zoom_factor = ZoomFactor::new(match self.scale_mode {
                ScaleMode::None => 1.0 / s,
                ScaleMode::Shrink => {
                    if s < 1.0 {
                        1.0
                    } else {
                        1.0 / s
                    }
                }
                ScaleMode::Full => 1.0,
                ScaleMode::Crop => {
                    let (widget_w_px, widget_h_px) = self.viewport_pixel_size();
                    let s_w = widget_w_px / self.img_width as f64;
                    let s_h = widget_h_px / self.img_height as f64;
                    s_w.max(s_h) / s
                }
            });
            self.pan_offset = PanOffset::ZERO;
            self.resize_state.mark_dirty();
            self.screen_clear.defer_until_protocol();
        }
    }

    /// Cycles through the ScaleModes (None -> Shrink -> Full -> Crop).
    pub fn cycle_scale_mode(&mut self) {
        if self.is_loading() {
            return;
        }
        self.scale_mode = match self.scale_mode {
            ScaleMode::None => ScaleMode::Shrink,
            ScaleMode::Shrink => ScaleMode::Full,
            ScaleMode::Full => ScaleMode::Crop,
            ScaleMode::Crop => ScaleMode::None,
        };
        self.apply_scale_mode();
    }

    /// Increments the brightness of the current image.
    pub fn increase_brightness(&mut self) {
        if self.original_image.is_none() || self.is_loading() {
            return;
        }
        let idx = self.queue.current_index;
        if let Some(adj) = self.queue.adjustments_mut().get_mut(idx) {
            let old = adj.brightness;
            adj.brightness.adjust(10);
            if old != adj.brightness {
                self.resize_state.mark_dirty();
            }
        }
    }

    /// Decrements the brightness of the current image.
    pub fn decrease_brightness(&mut self) {
        if self.original_image.is_none() || self.is_loading() {
            return;
        }
        let idx = self.queue.current_index;
        if let Some(adj) = self.queue.adjustments_mut().get_mut(idx) {
            let old = adj.brightness;
            adj.brightness.adjust(-10);
            if old != adj.brightness {
                self.resize_state.mark_dirty();
            }
        }
    }

    /// Increments the contrast of the current image.
    pub fn increase_contrast(&mut self) {
        if self.original_image.is_none() || self.is_loading() {
            return;
        }
        let idx = self.queue.current_index;
        if let Some(adj) = self.queue.adjustments_mut().get_mut(idx) {
            let mut next = adj.contrast;
            next.adjust(10.0);
            if adj.contrast.update(next.value()) {
                self.resize_state.mark_dirty();
            }
        }
    }

    /// Decrements the contrast of the current image.
    pub fn decrease_contrast(&mut self) {
        if self.original_image.is_none() || self.is_loading() {
            return;
        }
        let idx = self.queue.current_index;
        if let Some(adj) = self.queue.adjustments_mut().get_mut(idx) {
            let mut next = adj.contrast;
            next.adjust(-10.0);
            if adj.contrast.update(next.value()) {
                self.resize_state.mark_dirty();
            }
        }
    }

    /// Clamps the pan offset to ensure the viewport does not pan beyond safety margins.
    pub fn clamp_pan(&mut self) {
        if self.original_image.is_none() {
            return;
        }
        self.pan_offset.clamp(self.img_width, self.img_height);
    }

    /// Pans the image viewport along the given [`PanVector`].
    pub fn pan_by(&mut self, vec: PanVector) {
        if vec.is_zero() || self.original_image.is_none() || self.is_loading() {
            return;
        }
        let step_x = self.pan_step_x();
        let step_y = self.pan_step_y();
        self.pan_offset.x = self
            .pan_offset
            .x
            .saturating_add(vec.dx.signum().saturating_mul(step_x));
        self.pan_offset.y = self
            .pan_offset
            .y
            .saturating_add(vec.dy.signum().saturating_mul(step_y));
        self.clamp_pan();
        self.resize_state.mark_dirty();
    }

    /// Handles a keyboard pan press or repeat event using [`HeldPanState`].
    pub fn handle_pan_press(&mut self, dir: PanDirection) {
        let vec = self.held_pan.on_press(dir);
        self.pan_by(vec);
    }

    /// Pans the image viewport to the left.
    pub fn pan_left(&mut self) {
        self.pan_by(PanVector::from_direction(PanDirection::Left));
    }

    /// Pans the image viewport to the right.
    pub fn pan_right(&mut self) {
        self.pan_by(PanVector::from_direction(PanDirection::Right));
    }

    /// Pans the image viewport upwards.
    pub fn pan_up(&mut self) {
        self.pan_by(PanVector::from_direction(PanDirection::Up));
    }

    /// Pans the image viewport downwards.
    pub fn pan_down(&mut self) {
        self.pan_by(PanVector::from_direction(PanDirection::Down));
    }

    /// Calculates the horizontal panning step delta based on viewport cell width and zoom level.
    pub fn pan_step_x(&self) -> i64 {
        let (widget_w_px, _) = self.viewport_pixel_size();

        let s = self.get_fit_scale();
        let scale = if s > 0.0 {
            s * self.zoom_factor.value()
        } else {
            1.0
        };

        let step_px = if widget_w_px > 0.0 {
            widget_w_px * 0.05
        } else {
            self.img_width as f64 * 0.05
        };

        let step = step_px / scale;
        if step.is_nan() || step.is_infinite() {
            self.img_width as i64
        } else {
            (step.max(1.0) as i64).min(self.img_width as i64)
        }
    }

    /// Calculates the vertical panning step delta based on viewport cell height and zoom level.
    pub fn pan_step_y(&self) -> i64 {
        let (_, widget_h_px) = self.viewport_pixel_size();

        let s = self.get_fit_scale();
        let scale = if s > 0.0 {
            s * self.zoom_factor.value()
        } else {
            1.0
        };

        let step_px = if widget_h_px > 0.0 {
            widget_h_px * 0.05
        } else {
            self.img_height as f64 * 0.05
        };

        let step = step_px / scale;
        if step.is_nan() || step.is_infinite() {
            self.img_height as i64
        } else {
            (step.max(1.0) as i64).min(self.img_height as i64)
        }
    }

    /// Rotates the image clockwise or counter-clockwise by 90 degrees.
    fn rotate_90_step(&mut self, clockwise: bool) {
        if self.is_loading() {
            return;
        }
        if let Some(img) = self.original_image.take() {
            let rotated = if clockwise {
                rotate90_fast(&img)
            } else {
                rotate270_fast(&img)
            };
            let rotated_arc = Arc::new(rotated);

            if self.show_thumbnail_only || self.load_phase != LoadPhase::Complete {
                std::mem::swap(&mut self.img_width, &mut self.img_height);
            } else {
                self.img_width = rotated_arc.width();
                self.img_height = rotated_arc.height();
            }

            if let Some(thumb) = self.thumbnail_image.take() {
                let rotated_thumb = if Arc::ptr_eq(&img, &thumb) {
                    Arc::clone(&rotated_arc)
                } else if clockwise {
                    Arc::new(rotate90_fast(&thumb))
                } else {
                    Arc::new(rotate270_fast(&thumb))
                };
                if let ThumbnailState::Loaded(ref mut t) = self.stats.thumbnail {
                    t.dimensions = (rotated_thumb.width(), rotated_thumb.height());
                }
                self.thumbnail_image = Some(rotated_thumb);
            } else if let ThumbnailState::Loaded(ref mut t) = self.stats.thumbnail {
                std::mem::swap(&mut t.dimensions.0, &mut t.dimensions.1);
            }

            self.original_image = Some(rotated_arc);

            let idx = self.queue.current_index;
            if let Some(adj) = self.queue.adjustments_mut().get_mut(idx) {
                adj.rotation = if clockwise {
                    adj.rotation.rotate_clockwise()
                } else {
                    adj.rotation.rotate_counter_clockwise()
                };
            }

            self.zoom_factor = ZoomFactor::DEFAULT;
            self.pan_offset = PanOffset::ZERO;
            self.resize_state.mark_dirty();
            self.screen_clear.defer_until_protocol();
        }
    }

    /// Rotates the image clockwise by 90 degrees.
    pub fn rotate_clockwise(&mut self) {
        self.rotate_90_step(true);
    }

    /// Rotates the image counter-clockwise by 90 degrees.
    pub fn rotate_counter_clockwise(&mut self) {
        self.rotate_90_step(false);
    }
}
