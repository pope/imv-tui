use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use image::{DynamicImage, GenericImage, imageops::FilterType};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Style, Stylize},
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph},
};
use ratatui_image::{
    StatefulImage,
    picker::{Picker, ProtocolType},
    protocol::StatefulProtocol,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PaletteMode {
    Closed,
    Command,
    File,
}

pub struct CommandItem {
    pub name: &'static str,
    pub description: &'static str,
}

const COMMANDS: &[CommandItem] = &[
    CommandItem {
        name: "Show Help",
        description: "Show keyboard shortcuts dialog",
    },
    CommandItem {
        name: "Reset View",
        description: "Fit image to screen and reset panning",
    },
    CommandItem {
        name: "Actual Size",
        description: "Zoom image to 1:1 pixel scale (100%)",
    },
    CommandItem {
        name: "Rotate Clockwise",
        description: "Rotate image 90 degrees clockwise",
    },
    CommandItem {
        name: "Rotate Counter-Clockwise",
        description: "Rotate image 90 degrees counter-clockwise",
    },
    CommandItem {
        name: "Next Image",
        description: "Switch to the next image in directory",
    },
    CommandItem {
        name: "Previous Image",
        description: "Switch to the previous image in directory",
    },
    CommandItem {
        name: "Zoom In",
        description: "Zoom in closer",
    },
    CommandItem {
        name: "Zoom Out",
        description: "Zoom out further",
    },
    CommandItem {
        name: "Quit",
        description: "Close the application",
    },
    CommandItem {
        name: "Set Filter: Nearest",
        description: "Use Nearest Neighbor scaling (sharp, fast)",
    },
    CommandItem {
        name: "Set Filter: Linear",
        description: "Use Bilinear/Triangle scaling (smooth)",
    },
    CommandItem {
        name: "Set Filter: Cubic",
        description: "Use Bicubic/Catmull-Rom scaling (very smooth)",
    },
    CommandItem {
        name: "Set Filter: Gaussian",
        description: "Use Gaussian scaling (smooth, blurred)",
    },
    CommandItem {
        name: "Set Filter: Lanczos",
        description: "Use Lanczos3 scaling (highest quality)",
    },
];

fn fuzzy_match(text: &str, query: &str) -> bool {
    let text_lower = text.to_lowercase();
    let query_lower = query.to_lowercase();
    let mut text_chars = text_lower.chars();
    for q_char in query_lower.chars() {
        if !text_chars.any(|t_char| t_char == q_char) {
            return false;
        }
    }
    true
}

/// App state
pub struct App {
    pub images: Vec<PathBuf>,
    pub current_index: usize,
    pub original_image: Option<DynamicImage>,
    pub image_protocol: Option<StatefulProtocol>,
    pub picker: Picker,

    pub img_width: u32,
    pub img_height: u32,

    pub zoom_factor: f64,
    pub pan_offset: (i64, i64),

    pub running: bool,
    pub show_help: bool,
    pub error_message: Option<String>,

    pub last_widget_size: (u16, u16),
    pub needs_update: bool,
    pub needs_clear: bool,
    pub rendered_size_cells: (u16, u16),
    pub current_zoom_pct: f64,
    pub palette_mode: PaletteMode,
    pub palette_query: String,
    pub palette_selected_index: usize,
    pub filter_type: FilterType,
}

impl App {
    pub fn new(
        initial_path: &Path,
        picker: Picker,
        filter_type: FilterType,
    ) -> Result<Self, String> {
        let (images, current_index) = scan_directory(initial_path)?;

        let mut app = Self {
            images,
            current_index,
            original_image: None,
            image_protocol: None,
            picker,
            img_width: 0,
            img_height: 0,
            zoom_factor: 1.0,
            pan_offset: (0, 0),
            running: true,
            show_help: false,
            error_message: None,
            last_widget_size: (0, 0),
            needs_update: true,
            needs_clear: true,
            rendered_size_cells: (0, 0),
            current_zoom_pct: 100.0,
            palette_mode: PaletteMode::Closed,
            palette_query: String::new(),
            palette_selected_index: 0,
            filter_type,
        };

        app.load_image();
        Ok(app)
    }

    pub fn get_filtered_files(&self) -> Vec<(usize, String)> {
        self.images
            .iter()
            .enumerate()
            .map(|(idx, path)| {
                (
                    idx,
                    path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("Unknown")
                        .to_string(),
                )
            })
            .filter(|(_, name)| fuzzy_match(name, &self.palette_query))
            .collect()
    }

    pub fn get_filtered_commands(&self) -> Vec<&'static CommandItem> {
        COMMANDS
            .iter()
            .filter(|cmd| {
                fuzzy_match(cmd.name, &self.palette_query)
                    || fuzzy_match(cmd.description, &self.palette_query)
            })
            .collect()
    }

    pub fn filter_name(&self) -> &'static str {
        match self.filter_type {
            FilterType::Nearest => "Nearest",
            FilterType::Triangle => "Linear",
            FilterType::CatmullRom => "Cubic",
            FilterType::Gaussian => "Gaussian",
            FilterType::Lanczos3 => "Lanczos",
        }
    }

    pub fn execute_command(&mut self, name: &str) {
        match name {
            "Show Help" => {
                self.show_help = true;
                self.needs_update = true;
                self.needs_clear = true;
            }
            "Reset View" => self.reset_view(),
            "Actual Size" => self.set_actual_size(),
            "Rotate Clockwise" => self.rotate_clockwise(),
            "Rotate Counter-Clockwise" => self.rotate_counter_clockwise(),
            "Next Image" => self.next_image(),
            "Previous Image" => self.prev_image(),
            "Zoom In" => self.zoom_in(),
            "Zoom Out" => self.zoom_out(),
            "Quit" => self.running = false,
            "Set Filter: Nearest" => {
                self.filter_type = FilterType::Nearest;
                self.needs_update = true;
            }
            "Set Filter: Linear" => {
                self.filter_type = FilterType::Triangle;
                self.needs_update = true;
            }
            "Set Filter: Cubic" => {
                self.filter_type = FilterType::CatmullRom;
                self.needs_update = true;
            }
            "Set Filter: Gaussian" => {
                self.filter_type = FilterType::Gaussian;
                self.needs_update = true;
            }
            "Set Filter: Lanczos" => {
                self.filter_type = FilterType::Lanczos3;
                self.needs_update = true;
            }
            _ => {}
        }
    }

    /// Load the image at the current index
    pub fn load_image(&mut self) {
        if self.images.is_empty() {
            self.original_image = None;
            self.image_protocol = None;
            self.error_message = Some("No supported images found".to_string());
            return;
        }

        let path = &self.images[self.current_index];
        match image::ImageReader::open(path) {
            Ok(reader) => match reader.decode() {
                Ok(img) => {
                    self.img_width = img.width();
                    self.img_height = img.height();
                    self.original_image = Some(img);
                    self.error_message = None;
                    self.zoom_factor = 1.0;
                    self.pan_offset = (0, 0);
                    self.needs_update = true;
                    self.needs_clear = true;
                }
                Err(e) => {
                    self.original_image = None;
                    self.image_protocol = None;
                    self.error_message = Some(format!(
                        "Failed to decode image:\n{}\n\nError: {}",
                        path.display(),
                        e
                    ));
                }
            },
            Err(e) => {
                self.original_image = None;
                self.image_protocol = None;
                self.error_message = Some(format!(
                    "Failed to open file:\n{}\n\nError: {}",
                    path.display(),
                    e
                ));
            }
        }
    }

    /// Update the ratatui-image protocol state based on zoom and pan
    pub fn update_protocol(&mut self, widget_w: u16, widget_h: u16) {
        if widget_w == 0 || widget_h == 0 {
            return;
        }

        let font_size = self.picker.font_size();
        let mut cell_w = font_size.width;
        let mut cell_h = font_size.height;
        if cell_w == 0 {
            cell_w = 8;
        }
        if cell_h == 0 {
            cell_h = 16;
        }

        let widget_w_px = widget_w as f64 * cell_w as f64;
        let widget_h_px = widget_h as f64 * cell_h as f64;

        if let Some(ref img) = self.original_image {
            let w_orig = self.img_width as f64;
            let h_orig = self.img_height as f64;

            // 1. Calculate fit-to-screen scale 's'
            let s_w = widget_w_px / w_orig;
            let s_h = widget_h_px / h_orig;
            let s = s_w.min(s_h);

            // 2. Combined scale is s * zoom_factor
            let scale = s * self.zoom_factor;
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
            let center_x = (self.img_width as i64 / 2) + self.pan_offset.0;
            let center_y = (self.img_height as i64 / 2) + self.pan_offset.1;

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

            let canvas = if inter_x1 == crop_x1
                && inter_x2 == crop_x2
                && inter_y1 == crop_y1
                && inter_y2 == crop_y2
            {
                // Optimization: Crop box is fully inside the image (e.g. zoomed in & panning).
                // Resize the crop directly, completely bypassing background canvas allocation and overlays!
                let cropped_part = img.crop_imm(
                    inter_x1 as u32,
                    inter_y1 as u32,
                    (inter_x2 - inter_x1) as u32,
                    (inter_y2 - inter_y1) as u32,
                );
                cropped_part.resize(target_w, target_h, self.filter_type)
            } else {
                // Crop box goes outside image bounds (e.g. zoomed out or panned past edge).
                // Create a blank background canvas and copy the visible portion onto it.
                let mut screen_canvas = image::RgbaImage::new(target_w, target_h);

                if inter_x2 > inter_x1 && inter_y2 > inter_y1 {
                    let cropped_part = img.crop_imm(
                        inter_x1 as u32,
                        inter_y1 as u32,
                        (inter_x2 - inter_x1) as u32,
                        (inter_y2 - inter_y1) as u32,
                    );

                    let target_inter_w =
                        (((inter_x2 - inter_x1) as f64 * scale).round() as u32).max(1);
                    let target_inter_h =
                        (((inter_y2 - inter_y1) as f64 * scale).round() as u32).max(1);

                    let resized_part =
                        cropped_part.resize(target_inter_w, target_inter_h, self.filter_type);

                    let paste_x = ((inter_x1 - crop_x1) as f64 * scale).round() as i64;
                    let paste_y = ((inter_y1 - crop_y1) as f64 * scale).round() as i64;

                    let paste_x =
                        paste_x.clamp(0, (target_w as i64 - target_inter_w as i64).max(0)) as u32;
                    let paste_y =
                        paste_y.clamp(0, (target_h as i64 - target_inter_h as i64).max(0)) as u32;

                    // Fast memory-copy block transfer without expensive alpha-blending math
                    let _ = screen_canvas.copy_from(&resized_part, paste_x, paste_y);
                }
                DynamicImage::ImageRgba8(screen_canvas)
            };

            self.image_protocol = Some(self.picker.new_resize_protocol(canvas));

            // Calculate exact cell size of the rendered image
            let cells_w = (target_w as f64 / cell_w as f64).round() as u16;
            let cells_h = (target_h as f64 / cell_h as f64).round() as u16;
            self.rendered_size_cells = (cells_w.clamp(1, widget_w), cells_h.clamp(1, widget_h));
        } else {
            self.image_protocol = None;
            self.rendered_size_cells = (0, 0);
        }
    }

    fn get_fit_scale(&self) -> f64 {
        let (widget_w_cells, widget_h_cells) = self.last_widget_size;
        if widget_w_cells == 0 || widget_h_cells == 0 {
            return 0.0;
        }
        let font_size = self.picker.font_size();
        let mut cell_w = font_size.width;
        let mut cell_h = font_size.height;
        if cell_w == 0 {
            cell_w = 8;
        }
        if cell_h == 0 {
            cell_h = 16;
        }

        let widget_w_px = widget_w_cells as f64 * cell_w as f64;
        let widget_h_px = widget_h_cells as f64 * cell_h as f64;

        let s_w = widget_w_px / self.img_width as f64;
        let s_h = widget_h_px / self.img_height as f64;
        s_w.min(s_h)
    }

    /// Detect if we should sweep the screen with clear() to avoid graphics overlap artifacts.
    /// Only necessary for Sixel terminals (like Foot) which write directly to cell grids.
    pub fn should_clear_on_update(&self) -> bool {
        matches!(self.picker.protocol_type(), ProtocolType::Sixel)
    }

    /// Zoom in
    pub fn zoom_in(&mut self) {
        if self.original_image.is_none() {
            return;
        }
        let s = self.get_fit_scale();
        if s > 0.0 {
            self.zoom_factor = (self.zoom_factor * 1.25).min(100.0 / s);
            self.clamp_pan();
            self.needs_update = true;
        }
    }

    /// Zoom out
    pub fn zoom_out(&mut self) {
        if self.original_image.is_none() {
            return;
        }
        let s = self.get_fit_scale();
        if s > 0.0 {
            self.zoom_factor = (self.zoom_factor / 1.25).max(0.01 / s);
            self.clamp_pan();
            self.needs_update = true;
        }
    }

    /// Actual size (100% zoom)
    pub fn set_actual_size(&mut self) {
        if self.original_image.is_none() {
            return;
        }
        let s = self.get_fit_scale();
        if s > 0.0 {
            // Actual size scale is 1.0 (100%). Since scale = s * zoom_factor,
            // we want s * zoom_factor = 1.0 => zoom_factor = 1.0 / s
            self.zoom_factor = 1.0 / s;
            self.clamp_pan();
            self.needs_update = true;
            self.needs_clear = true;
        }
    }

    /// Reset zoom and pan
    pub fn reset_view(&mut self) {
        if self.original_image.is_none() {
            return;
        }
        self.zoom_factor = 1.0;
        self.pan_offset = (0, 0);
        self.needs_update = true;
        self.needs_clear = true;
    }

    /// Clamp pan offsets so that the corners of the image cannot pan past the center point of the viewport
    pub fn clamp_pan(&mut self) {
        if self.original_image.is_none() {
            return;
        }
        let max_pan_x = (self.img_width as i64 / 2).max(0);
        let max_pan_y = (self.img_height as i64 / 2).max(0);

        self.pan_offset.0 = self.pan_offset.0.clamp(-max_pan_x, max_pan_x);
        self.pan_offset.1 = self.pan_offset.1.clamp(-max_pan_y, max_pan_y);
    }

    /// Pan left
    pub fn pan_left(&mut self) {
        let step = self.pan_step_x();
        self.pan_offset.0 -= step;
        self.clamp_pan();
        self.needs_update = true;
    }

    /// Pan right
    pub fn pan_right(&mut self) {
        let step = self.pan_step_x();
        self.pan_offset.0 += step;
        self.clamp_pan();
        self.needs_update = true;
    }

    /// Pan up
    pub fn pan_up(&mut self) {
        let step = self.pan_step_y();
        self.pan_offset.1 -= step;
        self.clamp_pan();
        self.needs_update = true;
    }

    /// Pan down
    pub fn pan_down(&mut self) {
        let step = self.pan_step_y();
        self.pan_offset.1 += step;
        self.clamp_pan();
        self.needs_update = true;
    }

    fn pan_step_x(&self) -> i64 {
        let s = self.get_fit_scale();
        let scale = if s > 0.0 { s * self.zoom_factor } else { 1.0 };
        ((self.img_width as f64 * 0.05) / scale).max(1.0) as i64
    }

    fn pan_step_y(&self) -> i64 {
        let s = self.get_fit_scale();
        let scale = if s > 0.0 { s * self.zoom_factor } else { 1.0 };
        ((self.img_height as f64 * 0.05) / scale).max(1.0) as i64
    }

    /// Rotate 90 degrees clockwise
    pub fn rotate_clockwise(&mut self) {
        if let Some(img) = self.original_image.take() {
            let rotated = img.rotate90();
            self.img_width = rotated.width();
            self.img_height = rotated.height();
            self.original_image = Some(rotated);
            self.zoom_factor = 1.0;
            self.pan_offset = (0, 0);
            self.needs_update = true;
            self.needs_clear = true;
        }
    }

    /// Rotate 90 degrees counter-clockwise
    pub fn rotate_counter_clockwise(&mut self) {
        if let Some(img) = self.original_image.take() {
            let rotated = img.rotate270();
            self.img_width = rotated.width();
            self.img_height = rotated.height();
            self.original_image = Some(rotated);
            self.zoom_factor = 1.0;
            self.pan_offset = (0, 0);
            self.needs_update = true;
            self.needs_clear = true;
        }
    }

    /// Get current image file name
    pub fn current_filename(&self) -> String {
        if self.images.is_empty() {
            return "No file loaded".to_string();
        }
        self.images[self.current_index]
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Unknown")
            .to_string()
    }

    /// Next image
    pub fn next_image(&mut self) {
        if self.images.is_empty() {
            return;
        }
        self.current_index = (self.current_index + 1) % self.images.len();
        self.load_image();
    }

    /// Previous image
    pub fn prev_image(&mut self) {
        if self.images.is_empty() {
            return;
        }
        if self.current_index == 0 {
            self.current_index = self.images.len() - 1;
        } else {
            self.current_index -= 1;
        }
        self.load_image();
    }
}

/// Scan target path and sibling/child images
fn scan_directory(initial_path: &Path) -> Result<(Vec<PathBuf>, usize), String> {
    let (dir, file_name) = if initial_path.is_file() {
        let parent = initial_path.parent().unwrap_or_else(|| Path::new("."));
        let name = initial_path.file_name().map(|n| n.to_os_string());
        (parent.to_path_buf(), name)
    } else if initial_path.is_dir() {
        (initial_path.to_path_buf(), None)
    } else {
        // Fallback to checking if it is in relative path
        let parent = Path::new(".");
        if parent.join(initial_path).is_file() {
            (
                parent.to_path_buf(),
                Some(initial_path.as_os_str().to_os_string()),
            )
        } else {
            return Err(format!("Path does not exist: {}", initial_path.display()));
        }
    };

    let mut images = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && let Some(ext) = path.extension().and_then(|e| e.to_str())
            {
                let ext_lower = ext.to_lowercase();
                if matches!(
                    ext_lower.as_str(),
                    "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tiff" | "ico"
                ) {
                    images.push(path);
                }
            }
        }
    }

    images.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

    let index = if let Some(ref target_name) = file_name {
        images
            .iter()
            .position(|path| path.file_name().map(|n| n == target_name).unwrap_or(false))
            .unwrap_or_else(|| {
                if initial_path.exists() {
                    images.push(initial_path.to_path_buf());
                    images.len() - 1
                } else {
                    0
                }
            })
    } else {
        0
    };

    Ok((images, index))
}

fn ui(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(frame.area());

    // Update protocol if size has changed or update is requested
    let widget_size = (chunks[0].width, chunks[0].height);
    if app.needs_update || app.last_widget_size != widget_size {
        app.last_widget_size = widget_size;
        app.needs_update = false;
        app.update_protocol(widget_size.0, widget_size.1);
    }

    // Render image or placeholders
    if let Some(ref mut protocol) = app.image_protocol {
        // Calculate the centered Rect inside chunks[0]
        let (rect_w, rect_h) = app.rendered_size_cells;
        let x = chunks[0].x + (chunks[0].width.saturating_sub(rect_w)) / 2;
        let y = chunks[0].y + (chunks[0].height.saturating_sub(rect_h)) / 2;
        let centered_rect = Rect::new(x, y, rect_w, rect_h);

        let image_widget = StatefulImage::default();
        frame.render_stateful_widget(image_widget, centered_rect, protocol);
    } else if let Some(ref err) = app.error_message {
        let err_block = Block::default()
            .borders(Borders::ALL)
            .title(" Error Loading Image ")
            .style(Style::default().fg(Color::Red));
        let err_paragraph = Paragraph::new(err.as_str())
            .block(err_block)
            .alignment(Alignment::Center);
        frame.render_widget(err_paragraph, chunks[0]);
    } else {
        let loading_paragraph = Paragraph::new("No image loaded.")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::Yellow));
        frame.render_widget(loading_paragraph, chunks[0]);
    }

    // Render status bar
    let status_text = if app.images.is_empty() {
        " No files found. Press 'q' to quit. ".to_string()
    } else {
        format!(
            " [{}/{}] {} ({}x{}) | Filter: {} | Zoom: {}% | Pan: ({}, {}) | Press '?' for help ",
            app.current_index + 1,
            app.images.len(),
            app.current_filename(),
            app.img_width,
            app.img_height,
            app.filter_name(),
            app.current_zoom_pct.round() as i64,
            app.pan_offset.0,
            app.pan_offset.1
        )
    };

    let status_bar =
        Paragraph::new(status_text).style(Style::default().fg(Color::Black).bg(Color::Cyan));
    frame.render_widget(status_bar, chunks[1]);

    // Help Popup overlay
    if app.show_help {
        let help_lines = vec![
            Line::from(" imv-tui Keyboard Shortcuts ".bold().yellow()),
            Line::from(" ───────────────────────────────── ".gray()),
            Line::from(vec!["  q, Esc         ".cyan(), "- Quit".into()]),
            Line::from(vec!["  n, Space, ]    ".cyan(), "- Next image".into()]),
            Line::from(vec!["  p, Backspace, [".cyan(), "- Previous image".into()]),
            Line::from(vec!["  i, +           ".cyan(), "- Zoom In".into()]),
            Line::from(vec!["  o, -           ".cyan(), "- Zoom Out".into()]),
            Line::from(vec!["  a              ".cyan(), "- Actual Size".into()]),
            Line::from(vec!["  r              ".cyan(), "- Reset View".into()]),
            Line::from(vec![
                "  h, j, k, l     ".cyan(),
                "- Pan Left/Down/Up/Right".into(),
            ]),
            Line::from(vec![
                "  Arrow Keys     ".cyan(),
                "- Pan or Prev/Next image".into(),
            ]),
            Line::from(vec!["  e, R, >        ".cyan(), "- Rotate CW 90°".into()]),
            Line::from(vec!["  E, <           ".cyan(), "- Rotate CCW 90°".into()]),
            Line::from(vec!["  :              ".cyan(), "- Command Palette".into()]),
            Line::from(vec!["  f              ".cyan(), "- File Search".into()]),
            Line::from(vec!["  Mouse Scroll   ".cyan(), "- Zoom In / Out".into()]),
            Line::from(vec!["  ?, /           ".cyan(), "- Toggle Help".into()]),
        ];

        let help_paragraph = Paragraph::new(help_lines)
            .block(
                Block::default()
                    .title(" Help ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .style(Style::default().fg(Color::White).bg(Color::Reset));

        let help_width = 44_u16;
        let help_height = 19_u16;

        let w = help_width.min(chunks[0].width.saturating_sub(1));
        let h = help_height.min(chunks[0].height.saturating_sub(1));
        let x = chunks[0].x + chunks[0].width.saturating_sub(w).saturating_sub(1);
        let y = chunks[0].y.saturating_add(1);

        let popup_area = Rect::new(x, y, w, h);
        frame.render_widget(Clear, popup_area);
        frame.render_widget(help_paragraph, popup_area);
    }

    // Command / File Palette popup
    if app.palette_mode != PaletteMode::Closed {
        let title = match app.palette_mode {
            PaletteMode::File => " File Search ",
            PaletteMode::Command => " Command Palette ",
            _ => "",
        };

        let mut lines = vec![
            Line::from(vec![
                " > ".bold().cyan(),
                app.palette_query.as_str().into(),
                "▊".cyan(), // cursor block
            ]),
            Line::from("──────────────────────────────────────────────────────────".gray()),
        ];

        match app.palette_mode {
            PaletteMode::File => {
                let filtered_files = app.get_filtered_files();
                if !filtered_files.is_empty() {
                    app.palette_selected_index =
                        app.palette_selected_index.min(filtered_files.len() - 1);
                } else {
                    app.palette_selected_index = 0;
                }

                let total_files = filtered_files.len();
                let visible_count = 8;
                let start_idx = if total_files <= visible_count || app.palette_selected_index < 4 {
                    0
                } else if app.palette_selected_index >= total_files - 4 {
                    total_files - visible_count
                } else {
                    app.palette_selected_index - 4
                };

                for (i, (_, filename)) in filtered_files
                    .iter()
                    .enumerate()
                    .skip(start_idx)
                    .take(visible_count)
                {
                    let mut line = Line::from(format!("   {}", filename));
                    if i == app.palette_selected_index {
                        line = Line::from(format!(" > {}", filename))
                            .bold()
                            .yellow()
                            .on_blue();
                    }
                    lines.push(line);
                }

                if filtered_files.is_empty() {
                    lines.push(Line::from("   No matches found.".gray().italic()));
                }
            }
            PaletteMode::Command => {
                let filtered_commands = app.get_filtered_commands();
                if !filtered_commands.is_empty() {
                    app.palette_selected_index =
                        app.palette_selected_index.min(filtered_commands.len() - 1);
                } else {
                    app.palette_selected_index = 0;
                }

                let total_cmds = filtered_commands.len();
                let visible_count = 8;
                let start_idx = if total_cmds <= visible_count || app.palette_selected_index < 4 {
                    0
                } else if app.palette_selected_index >= total_cmds - 4 {
                    total_cmds - visible_count
                } else {
                    app.palette_selected_index - 4
                };

                for (i, cmd) in filtered_commands
                    .iter()
                    .enumerate()
                    .skip(start_idx)
                    .take(visible_count)
                {
                    let cmd_line = vec![
                        if i == app.palette_selected_index {
                            " > "
                        } else {
                            "   "
                        }
                        .into(),
                        cmd.name.bold(),
                        " - ".into(),
                        cmd.description.gray(),
                    ];
                    let mut line = Line::from(cmd_line);
                    if i == app.palette_selected_index {
                        line = line.yellow().on_blue();
                    }
                    lines.push(line);
                }

                if filtered_commands.is_empty() {
                    lines.push(Line::from("   No matches found.".gray().italic()));
                }
            }
            _ => {}
        }

        let palette_block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));

        let palette_paragraph = Paragraph::new(lines)
            .block(palette_block)
            .style(Style::default().fg(Color::White).bg(Color::Reset));

        let palette_width = 60_u16;
        let palette_height = 12_u16;

        let w = palette_width.min(chunks[0].width.saturating_sub(1));
        let h = palette_height.min(chunks[0].height.saturating_sub(1));
        let x = chunks[0].x + chunks[0].width.saturating_sub(w).saturating_sub(1);
        let y = chunks[0].y.saturating_add(1);

        let popup_area = Rect::new(x, y, w, h);

        frame.render_widget(Clear, popup_area);
        frame.render_widget(palette_paragraph, popup_area);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse arguments
    let args: Vec<String> = env::args().collect();
    let mut initial_path = None;
    let mut filter_opt = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--filter" | "-f" => {
                if i + 1 < args.len() {
                    filter_opt = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    eprintln!(
                        "Error: --filter / -f requires an argument (nearest, linear, cubic, gaussian, lanczos)"
                    );
                    std::process::exit(1);
                }
            }
            "--help" | "-h" => {
                println!("imv-tui: A fast keyboard-driven terminal image viewer");
                println!();
                println!("Usage: imv-tui [path] [options]");
                println!();
                println!("Options:");
                println!(
                    "  -f, --filter <filter>  Initial image scaling filter: nearest, linear, cubic, gaussian, lanczos"
                );
                println!("  -h, --help             Show this help menu");
                std::process::exit(0);
            }
            val => {
                if initial_path.is_none() {
                    initial_path = Some(PathBuf::from(val));
                }
                i += 1;
            }
        }
    }

    let initial_path = initial_path.unwrap_or_else(|| PathBuf::from("."));

    let initial_filter = match filter_opt.as_deref() {
        Some("nearest") => FilterType::Nearest,
        Some("linear") => FilterType::Triangle,
        Some("cubic") => FilterType::CatmullRom,
        Some("gaussian") => FilterType::Gaussian,
        Some("lanczos") => FilterType::Lanczos3,
        Some(other) => {
            eprintln!(
                "Error: Unknown filter '{}'. Choose from: nearest, linear, cubic, gaussian, lanczos",
                other
            );
            std::process::exit(1);
        }
        None => FilterType::Nearest,
    };

    // Query terminal protocol before raw mode
    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app
    let mut app = match App::new(&initial_path, picker, initial_filter) {
        Ok(app) => app,
        Err(e) => {
            // Restore terminal on init error
            disable_raw_mode()?;
            execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture)?;
            eprintln!("Initialization Error: {}", e);
            std::process::exit(1);
        }
    };

    // Main event loop
    while app.running {
        if app.needs_clear {
            app.needs_clear = false;
            if app.should_clear_on_update() {
                terminal.clear()?;
                app.needs_update = true;
            }
        }
        terminal.draw(|f| ui(f, &mut app))?;

        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if app.palette_mode != PaletteMode::Closed {
                        match key.code {
                            KeyCode::Esc => {
                                app.palette_mode = PaletteMode::Closed;
                                app.needs_update = true;
                                app.needs_clear = true;
                            }
                            KeyCode::Enter => {
                                match app.palette_mode {
                                    PaletteMode::File => {
                                        let files = app.get_filtered_files();
                                        if !files.is_empty()
                                            && app.palette_selected_index < files.len()
                                        {
                                            app.current_index = files[app.palette_selected_index].0;
                                            app.load_image();
                                        }
                                    }
                                    PaletteMode::Command => {
                                        let cmds = app.get_filtered_commands();
                                        if !cmds.is_empty()
                                            && app.palette_selected_index < cmds.len()
                                        {
                                            let cmd_name = cmds[app.palette_selected_index].name;
                                            app.execute_command(cmd_name);
                                        }
                                    }
                                    _ => {}
                                }
                                app.palette_mode = PaletteMode::Closed;
                                app.needs_update = true;
                                app.needs_clear = true;
                            }
                            KeyCode::Up if app.palette_selected_index > 0 => {
                                app.palette_selected_index -= 1;
                            }
                            KeyCode::Down => {
                                let max_len = match app.palette_mode {
                                    PaletteMode::File => app.get_filtered_files().len(),
                                    PaletteMode::Command => app.get_filtered_commands().len(),
                                    _ => 0,
                                };
                                if max_len > 0 && app.palette_selected_index < max_len - 1 {
                                    app.palette_selected_index += 1;
                                }
                            }
                            KeyCode::Char('k')
                                if key.modifiers.contains(event::KeyModifiers::CONTROL)
                                    && app.palette_selected_index > 0 =>
                            {
                                app.palette_selected_index -= 1;
                            }
                            KeyCode::Char('j')
                                if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                            {
                                let max_len = match app.palette_mode {
                                    PaletteMode::File => app.get_filtered_files().len(),
                                    PaletteMode::Command => app.get_filtered_commands().len(),
                                    _ => 0,
                                };
                                if max_len > 0 && app.palette_selected_index < max_len - 1 {
                                    app.palette_selected_index += 1;
                                }
                            }
                            KeyCode::Backspace => {
                                app.palette_query.pop();
                                app.palette_selected_index = 0;
                            }
                            KeyCode::Char(c) => {
                                app.palette_query.push(c);
                                app.palette_selected_index = 0;
                            }
                            _ => {}
                        }
                    } else {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => {
                                if app.show_help {
                                    app.show_help = false;
                                    app.needs_update = true;
                                    app.needs_clear = true;
                                } else {
                                    app.running = false;
                                }
                            }
                            KeyCode::Char('?') | KeyCode::Char('/') => {
                                app.show_help = !app.show_help;
                                app.needs_update = true;
                                app.needs_clear = true;
                            }
                            // Command Palette
                            KeyCode::Char(':') => {
                                app.palette_mode = PaletteMode::Command;
                                app.palette_query.clear();
                                app.palette_selected_index = 0;
                                app.needs_clear = true;
                            }
                            // File Palette
                            KeyCode::Char('f') => {
                                app.palette_mode = PaletteMode::File;
                                app.palette_query.clear();
                                app.palette_selected_index = app.current_index;
                                app.needs_clear = true;
                            }
                            // Next image
                            KeyCode::Char('n') | KeyCode::Char(' ') | KeyCode::Char(']') => {
                                app.next_image();
                            }
                            // Prev image
                            KeyCode::Char('p') | KeyCode::Char('[') | KeyCode::Backspace => {
                                app.prev_image();
                            }
                            // Zoom
                            KeyCode::Char('i') | KeyCode::Char('+') | KeyCode::Char('=') => {
                                app.zoom_in();
                            }
                            KeyCode::Char('o') | KeyCode::Char('-') => {
                                app.zoom_out();
                            }
                            // Actual size
                            KeyCode::Char('a') => {
                                app.set_actual_size();
                            }
                            // Reset
                            KeyCode::Char('r') => {
                                app.reset_view();
                            }
                            // Rotation
                            KeyCode::Char('e') | KeyCode::Char('R') | KeyCode::Char('>') => {
                                app.rotate_clockwise();
                            }
                            KeyCode::Char('E') | KeyCode::Char('<') => {
                                app.rotate_counter_clockwise();
                            }
                            // Vim Navigation (Pan)
                            KeyCode::Char('h') => {
                                app.pan_left();
                            }
                            KeyCode::Char('l') => {
                                app.pan_right();
                            }
                            KeyCode::Char('k') => {
                                app.pan_up();
                            }
                            KeyCode::Char('j') => {
                                app.pan_down();
                            }
                            // Arrow Keys (Pan)
                            KeyCode::Left => {
                                app.pan_left();
                            }
                            KeyCode::Right => {
                                app.pan_right();
                            }
                            KeyCode::Up => {
                                app.pan_up();
                            }
                            KeyCode::Down => {
                                app.pan_down();
                            }
                            _ => {}
                        }
                    }
                }
                Event::Mouse(mouse_event) => match mouse_event.kind {
                    MouseEventKind::ScrollUp => {
                        app.zoom_in();
                    }
                    MouseEventKind::ScrollDown => {
                        app.zoom_out();
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }

    // Cleanup terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}
