use crossterm::event::{Event, KeyCode, KeyEventKind};
use fast_image_resize as fir;
use image::DynamicImage;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::StatefulProtocol;
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::commands::{Command, PaletteCommand, get_commands};
use crate::image_worker::{
    Brightness, Contrast, CropBox, FilterType, ImageIntersection, ImageSource, LoaderRequest,
    LoaderResponse, PanOffset, ResizeRequest, ScaleMode, SlideshowConfig, process_resize,
};
/// Represents an absolute or relative adjustment to a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adjustment<T> {
    /// An absolute value assignment.
    Absolute(T),
    /// A relative addition to the current value.
    RelativeAdd(T),
    /// A relative subtraction from the current value.
    RelativeSub(T),
}

impl<T: std::str::FromStr> std::str::FromStr for Adjustment<T> {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err("Empty input".to_string());
        }
        if let Some(stripped) = s.strip_prefix('+') {
            let val = stripped
                .parse::<T>()
                .map_err(|_| "Invalid positive offset".to_string())?;
            Ok(Self::RelativeAdd(val))
        } else if let Some(stripped) = s.strip_prefix('-') {
            let val = stripped
                .parse::<T>()
                .map_err(|_| "Invalid negative offset".to_string())?;
            Ok(Self::RelativeSub(val))
        } else {
            let val = s
                .parse::<T>()
                .map_err(|_| "Invalid absolute value".to_string())?;
            Ok(Self::Absolute(val))
        }
    }
}

/// Individual image adjustments (brightness, contrast, rotation) that are preserved per-file.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ImageAdjustments {
    pub brightness: i32,
    pub contrast: f32,
    pub rotation: u32,
}

impl Default for ImageAdjustments {
    fn default() -> Self {
        Self {
            brightness: 0,
            contrast: 0.0,
            rotation: 0,
        }
    }
}

impl ImageAdjustments {
    /// Applies the rotation setting to the given DynamicImage.
    pub fn rotate_image(&self, img: &image::DynamicImage) -> image::DynamicImage {
        match self.rotation {
            90 => img.rotate90(),
            180 => img.rotate180(),
            270 => img.rotate270(),
            _ => img.clone(),
        }
    }
}

/// The classification/flagged state for an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Classification {
    Unflagged,
    Pick,
    Reject,
}

impl Classification {
    /// Returns the emoji icon representing the classification.
    pub fn icon(&self) -> &'static str {
        match self {
            Self::Unflagged => "⚪",
            Self::Pick => "⭐",
            Self::Reject => "❌",
        }
    }

    /// Returns the combined icon and label string (e.g. "⭐ Pick").
    pub fn display_label(&self) -> &'static str {
        match self {
            Self::Unflagged => "⚪ Unflagged",
            Self::Pick => "⭐ Pick",
            Self::Reject => "❌ Reject",
        }
    }

    /// Returns the short prefix for list views, leaving Unflagged blank for visual cleanliness.
    pub fn search_prefix(&self) -> &'static str {
        match self {
            Self::Unflagged => "   ",
            Self::Pick => "⭐ ",
            Self::Reject => "❌ ",
        }
    }
}

/// The display filtering view mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViewMode {
    /// Show picks and unflagged images (default).
    Default,
    /// Show only unflagged images.
    Unflagged,
    /// Show only picks.
    Picks,
    /// Show only rejects.
    Rejects,
    /// Show all files.
    All,
}

impl ViewMode {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Default => "Unflagged + Picks",
            Self::Unflagged => "Unflagged Only",
            Self::Picks => "Picks Only",
            Self::Rejects => "Rejects Only",
            Self::All => "All Files",
        }
    }
}

/// Helper struct for JSON serialization/deserialization of classifications.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ClassificationJsonItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive: Option<String>,
    pub filename: String,
    pub flag: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub brightness: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contrast: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation: Option<u32>,
}

/// Encapsulates list of images, starting index, and cached display name lists.
pub struct ImageQueue {
    /// Loaded list of image sources.
    pub images: Vec<ImageSource>,
    /// Pre-computed filename cache for standard status display.
    pub display_names: Vec<String>,
    /// Lowercase file name cache for case-insensitive matching.
    pub display_names_lowercase: Vec<String>,
    /// Current selected index in the images vector.
    pub current_index: usize,
}

impl ImageQueue {
    /// Creates a new ImageQueue, returning an error if the images list is empty.
    pub fn new(images: Vec<ImageSource>, current_index: usize) -> Result<Self, String> {
        if images.is_empty() {
            return Err("No supported images found".to_string());
        }
        let display_names: Vec<String> = images.iter().map(|img| img.display_name()).collect();
        let display_names_lowercase: Vec<String> = display_names
            .iter()
            .map(|name| name.to_lowercase())
            .collect();
        let current_index = current_index.min(images.len() - 1);
        Ok(Self {
            images,
            display_names,
            display_names_lowercase,
            current_index,
        })
    }

    /// Returns true if the image queue contains no images.
    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    /// Returns the filename display name of the currently selected image.
    pub fn get_current_filename(&self) -> &str {
        self.display_names
            .get(self.current_index)
            .map(|s| s.as_str())
            .unwrap_or("")
    }
}

/// The specific input prompt type.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PromptType {
    /// Go to specific image index.
    GoToImage,
    /// Adjust image brightness.
    SetBrightness,
    /// Adjust image contrast.
    SetContrast,
    /// Adjust slideshow interval.
    SetSlideshow,
}

/// The state of the top overlay search palette or prompt dialog.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PaletteMode {
    /// Palette/prompt overlay is closed.
    Closed,
    /// Searchable commands lookup.
    Command,
    /// Fuzzy search files in local queue.
    File,
    /// Prompt value input box is open.
    Prompt,
    /// Image metadata and statistics dialog is open.
    Info,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct StatsForNerds {
    /// Time taken to load (decode) the photo from disk or zip.
    pub load_duration: std::time::Duration,
    /// Time taken to extract and decode the thumbnail.
    pub thumbnail_load_duration: Option<std::time::Duration>,
    /// Dimensions of the decoded thumbnail placeholder, if any.
    pub thumbnail_dimensions: Option<(u32, u32)>,
    /// Was it loaded from the prefetch cache.
    pub is_prefetch_cache_hit: bool,
    /// Time taken to resize, apply filters, adjustments, etc.
    pub process_duration: std::time::Duration,
    /// Time taken to send it to the pixel handlers (writing to sixel or kitty APIs).
    pub protocol_duration: std::time::Duration,
    /// Target width in pixels sent to the graphics protocol.
    pub protocol_width: u32,
    /// Target height in pixels sent to the graphics protocol.
    pub protocol_height: u32,
    /// Size of the image on disk in bytes.
    pub disk_size: u64,
}

/// A cached image and its associated metadata in the prefetch cache.
#[derive(Clone)]
pub struct CachedImage {
    pub image: Option<Arc<DynamicImage>>,
    pub thumbnail: Option<Arc<DynamicImage>>,
    pub width: u32,
    pub height: u32,
    pub icon: &'static str,
    pub decode_duration: std::time::Duration,
    pub thumbnail_decode_duration: std::time::Duration,
    pub disk_size: u64,
}

type PrefetchCache = Arc<Mutex<HashMap<usize, CachedImage>>>;

/// A response payload sent by the resizing worker thread.
pub struct ResizeResponse {
    pub protocol: StatefulProtocol,
    pub rendered_cells: (u16, u16),
    pub process_duration: std::time::Duration,
    pub protocol_duration: std::time::Duration,
    pub target_width: u32,
    pub target_height: u32,
    pub sequence: u64,
}

/// The central application state controller.
pub struct App {
    /// The encapsulated image queue and navigation state.
    pub queue: ImageQueue,
    /// Cached filtered files matching current search query.
    pub filtered_files: Vec<(usize, String)>,
    /// Cached filtered commands matching current search query.
    pub filtered_commands: Vec<PaletteCommand>,
    /// Shared reference to the decoded dynamic image.
    pub original_image: Option<Arc<DynamicImage>>,
    /// Extracted thumbnail placeholder image, if any.
    pub thumbnail_image: Option<Arc<DynamicImage>>,
    /// If true, display the low-res thumbnail image placeholder only.
    pub show_thumbnail_only: bool,
    /// Active render state protocol (Kitty, Sixel, Halfblocks).
    pub image_protocol: Option<StatefulProtocol>,
    /// Target terminal graphics protocol picker.
    pub picker: Picker,

    /// Width in pixels of original decoded image.
    pub img_width: u32,
    /// Height in pixels of original decoded image.
    pub img_height: u32,

    /// Zoom multiplier relative to fit scale.
    pub zoom_factor: f64,
    /// Viewport panning coordinate offset.
    pub pan_offset: PanOffset,

    /// Application run lifecycle boolean.
    pub running: bool,
    /// Error message from loader thread if image decoding fails.
    pub error_message: Option<String>,

    /// Size of the image widget area on the last frame draw.
    pub last_widget_size: (u16, u16),
    /// If true, triggers a resize worker dispatch.
    pub needs_update: bool,
    /// Triggers a visual clearing of the terminal screen, but only if the active protocol requires it (e.g. Sixel).
    /// Used to clean Sixel cells on discrete updates (like image loads) without causing constant Kitty/Halfblocks flicker.
    pub needs_clear: bool,
    /// Triggers an unconditional visual clearing of the terminal text grid on the next frame.
    /// Primarily used to cleanly erase text characters of dismissed dialogues (Help, search palettes) from the image region
    /// in terminals using the Kitty graphics protocol (like WezTerm), where double buffering would otherwise skip/freeze them.
    pub needs_clear_once: bool,
    /// Cell dimensions of the actual rendered Sixel/Kitty image.
    pub rendered_size_cells: (u16, u16),
    /// Percentage display value of the current scaling.
    pub current_zoom_pct: f64,
    /// The active overlay panel state.
    pub palette_mode: PaletteMode,
    /// Input buffer for fuzzy queries or prompt values.
    pub palette_query: String,
    /// Selected row inside the fuzzy matcher list.
    pub palette_selected_index: usize,
    /// Freezed layout width of the popup dialogue.
    pub palette_width: u16,
    /// Dynamic height of the popup dialogue.
    pub palette_height: u16,
    /// The active prompt configuration details.
    pub prompt_type: Option<PromptType>,
    /// Desired scaling filter.
    pub filter_type: FilterType,
    /// Desired image layout scaling behavior.
    pub scale_mode: ScaleMode,
    /// The fuzzy matching search engine instance.
    pub matcher: nucleo::Matcher,
    /// Nerd font decoration icon matching the file extension.
    pub current_icon: &'static str,

    // Thread communication channels
    resize_tx: mpsc::Sender<ResizeRequest>,
    protocol_rx: mpsc::Receiver<ResizeResponse>,
    /// Loader thread dispatcher channel.
    pub loader_tx: mpsc::Sender<LoaderRequest>,
    response_rx: mpsc::Receiver<LoaderResponse>,
    current_sequence: u64,
    /// Loading state spinner indicator boolean.
    pub is_loading: bool,
    /// Timestamp when image loading started to debounce the spinner.
    pub loading_start_time: Option<Instant>,
    /// Defer screen clearing until protocol is received to prevent stutters.
    pub clear_on_protocol_receive: bool,
    /// Flag to force scale mode view recalculation on load.
    pub zoom_needs_initialization: bool,
    /// Active brightness bias value.
    pub brightness: Brightness,
    /// Active contrast bias value.
    pub contrast: Contrast,
    prefetch_cache: PrefetchCache,
    /// The slideshow transition configuration delay.
    pub slideshow_config: SlideshowConfig,
    /// Last slideshow transition timestamp.
    pub slideshow_last_transition: std::time::Instant,
    /// Stats for nerds instrumentation.
    pub stats: StatsForNerds,
    /// Last toggle timestamp of the info diagnostics overlay.
    pub last_info_toggle: Option<std::time::Instant>,
    /// Disable EXIF thumbnail rendering.
    pub disable_thumbnail: bool,
    /// Active view filtering mode.
    pub view_mode: ViewMode,
    /// Classification states for all loaded image sources, matching the index of queue.images.
    pub classifications: Vec<Classification>,
    /// Image adjustments (brightness, contrast, rotation) for each loaded image source.
    pub adjustments: Vec<ImageAdjustments>,
}

impl App {
    /// Creates a new App controller, launching background threadpools for both
    /// decoding/loading and resizing.
    pub fn new(
        images: Vec<ImageSource>,
        current_index: usize,
        picker: Picker,
        filter_type: FilterType,
        scale_mode: ScaleMode,
        disable_thumbnail: bool,
    ) -> Result<Self, String> {
        let queue = ImageQueue::new(images, current_index)?;

        let (resize_tx, resize_rx) = mpsc::channel::<ResizeRequest>();
        let (protocol_tx, protocol_rx) = mpsc::channel::<ResizeResponse>();

        // Spawn background resizing worker thread
        std::thread::spawn(move || {
            let mut resizer = fir::Resizer::new();
            while let Ok(req) = resize_rx.recv() {
                let mut latest_req = req;
                while let Ok(next_req) = resize_rx.try_recv() {
                    latest_req = next_req;
                }
                let rendered_cells = latest_req.rendered_size_cells;
                let target_w = latest_req.target_w;
                let target_h = latest_req.target_h;
                let sequence = latest_req.sequence;
                let (protocol, proc_dur, proto_dur) = process_resize(latest_req, &mut resizer);
                let _ = protocol_tx.send(ResizeResponse {
                    protocol,
                    rendered_cells,
                    process_duration: proc_dur,
                    protocol_duration: proto_dur,
                    target_width: target_w,
                    target_height: target_h,
                    sequence,
                });
            }
        });

        let (loader_tx, loader_rx) = mpsc::channel::<LoaderRequest>();
        let (response_tx, response_rx) = mpsc::channel::<LoaderResponse>();

        // Spawn persistent background loader thread
        std::thread::spawn(move || {
            while let Ok(req) = loader_rx.recv() {
                let mut requests = vec![req];
                while let Ok(r) = loader_rx.try_recv() {
                    requests.push(r);
                }

                // Find the highest sequence number
                let highest_seq = requests.iter().map(|r| r.sequence).max().unwrap_or(0);

                // Keep only requests matching the highest sequence
                let mut current_requests: Vec<LoaderRequest> = requests
                    .into_iter()
                    .filter(|r| r.sequence == highest_seq)
                    .collect();

                // Sort current_requests so that the active load (is_prefetch == false) is processed first
                current_requests.sort_by_key(|r| r.is_prefetch);

                for r in current_requests {
                    let start = std::time::Instant::now();

                    // Try to load limited bytes first and extract thumbnail if this is an active cold load
                    let mut bytes_opt = None;
                    let limit = 256 * 1024; // 256 KB limit to avoid massive full-file reads for metadata/thumbnails

                    if !disable_thumbnail {
                        let read_res =
                            crate::image_worker::read_source_bytes_limited(&r.source, limit);
                        if let Ok(partial_bytes) = read_res {
                            if let Some((thumb_img, real_w, real_h)) =
                                crate::image_worker::decode_thumbnail_and_dimensions(&partial_bytes)
                            {
                                // Send thumbnail placeholder immediately
                                let thumb_dur = start.elapsed();
                                let _ = response_tx.send(LoaderResponse {
                                    idx: r.idx,
                                    result: Ok((
                                        thumb_img,
                                        real_w,
                                        real_h,
                                        "\u{F0225}",
                                        0, // Filled in by final high-res response
                                    )),
                                    is_prefetch: r.is_prefetch,
                                    sequence: r.sequence,
                                    decode_duration: thumb_dur,
                                    is_thumbnail: true,
                                });
                            }
                            if partial_bytes.len() < limit {
                                // If the file was smaller than the limit, we have already loaded it fully
                                bytes_opt = Some(partial_bytes);
                            }
                        }
                    }

                    // Decode the full resolution image
                    let final_bytes = if let Some(bytes) = bytes_opt {
                        Some(bytes)
                    } else {
                        crate::image_worker::read_source_bytes(&r.source).ok()
                    };

                    let res = if let Some(ref bytes) = final_bytes {
                        crate::image_worker::decode_image_bytes(bytes, &r.source)
                    } else {
                        crate::image_worker::decode_image_source(r.source)
                    };

                    let decode_duration = start.elapsed();
                    let _ = response_tx.send(LoaderResponse {
                        idx: r.idx,
                        result: res,
                        is_prefetch: r.is_prefetch,
                        sequence: r.sequence,
                        decode_duration,
                        is_thumbnail: false,
                    });
                }
            }
        });

        let num_images = queue.images.len();
        let mut app = Self {
            queue,
            filtered_files: Vec::new(),
            filtered_commands: Vec::new(),
            original_image: None,
            thumbnail_image: None,
            show_thumbnail_only: false,
            image_protocol: None,
            picker,
            img_width: 0,
            img_height: 0,
            zoom_factor: 1.0,
            pan_offset: PanOffset::ZERO,
            running: true,
            error_message: None,
            last_widget_size: (0, 0),
            needs_update: true,
            needs_clear: true,
            needs_clear_once: false,
            rendered_size_cells: (0, 0),
            current_zoom_pct: 100.0,
            palette_mode: PaletteMode::Closed,
            palette_query: String::new(),
            palette_selected_index: 0,
            palette_width: 0,
            palette_height: 0,
            prompt_type: None,
            filter_type,
            scale_mode,
            matcher: nucleo::Matcher::new(nucleo::Config::DEFAULT),
            current_icon: "\u{F021F}",
            resize_tx,
            protocol_rx,
            loader_tx,
            response_rx,
            current_sequence: 0,
            is_loading: false,
            loading_start_time: None,
            clear_on_protocol_receive: false,
            zoom_needs_initialization: false,
            brightness: Brightness::ZERO,
            contrast: Contrast::ZERO,
            prefetch_cache: Arc::new(Mutex::new(HashMap::new())),
            slideshow_config: SlideshowConfig::OFF,
            slideshow_last_transition: std::time::Instant::now(),
            stats: StatsForNerds::default(),
            last_info_toggle: None,
            disable_thumbnail,
            view_mode: ViewMode::Default,
            classifications: vec![Classification::Unflagged; num_images],
            adjustments: vec![ImageAdjustments::default(); num_images],
        };

        app.start_load_image();
        Ok(app)
    }

    /// Checks if the image at index is visible under the current ViewMode.
    pub fn is_visible(&self, index: usize) -> bool {
        let classification = self
            .classifications
            .get(index)
            .cloned()
            .unwrap_or(Classification::Unflagged);
        match self.view_mode {
            ViewMode::Default => {
                classification == Classification::Pick
                    || classification == Classification::Unflagged
            }
            ViewMode::Unflagged => classification == Classification::Unflagged,
            ViewMode::Picks => classification == Classification::Pick,
            ViewMode::Rejects => classification == Classification::Reject,
            ViewMode::All => true,
        }
    }

    /// Returns the total number of visible images under the current view filter.
    pub fn get_visible_count(&self) -> usize {
        (0..self.queue.images.len())
            .filter(|&idx| self.is_visible(idx))
            .count()
    }

    /// Returns the 0-based position of the current image within the visible list.
    pub fn get_visible_position(&self) -> Option<usize> {
        let mut count = 0;
        for idx in 0..self.queue.images.len() {
            if self.is_visible(idx) {
                if idx == self.queue.current_index {
                    return Some(count);
                }
                count += 1;
            }
        }
        None
    }

    /// Finds the closest visible index starting at start_idx, checking current index, then scanning forward.
    pub fn find_closest_visible_index(&self, start_idx: usize) -> Option<usize> {
        let total = self.queue.images.len();
        if total == 0 {
            return None;
        }
        for i in 0..total {
            let idx = (start_idx + i) % total;
            if self.is_visible(idx) {
                return Some(idx);
            }
        }
        None
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
                // Trigger prefetching update as surrounding visible images might have changed
                self.trigger_prefetch();
            }
        } else {
            // No visible images matching the filter
            self.original_image = None;
            self.thumbnail_image = None;
            self.image_protocol = None;
            self.error_message = Some("No images match the current view filter".to_string());
            self.needs_update = true;
            self.needs_clear_once = true;
        }
    }

    /// Marks the current image as a Pick.
    pub fn mark_pick(&mut self) {
        if self.queue.images.is_empty() {
            return;
        }
        let idx = self.queue.current_index;
        self.classifications[idx] = Classification::Pick;
        self.adjust_current_index_for_visibility();
    }

    /// Marks the current image as a Reject.
    pub fn mark_reject(&mut self) {
        if self.queue.images.is_empty() {
            return;
        }
        let idx = self.queue.current_index;
        self.classifications[idx] = Classification::Reject;
        self.adjust_current_index_for_visibility();
    }

    /// Removes any pick/reject flags from the current image.
    pub fn unflag_image(&mut self) {
        if self.queue.images.is_empty() {
            return;
        }
        let idx = self.queue.current_index;
        self.classifications[idx] = Classification::Unflagged;
        self.adjust_current_index_for_visibility();
    }

    /// Cycles the active view filter mode.
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

    /// Sets the active view filter mode to a specific value.
    pub fn set_view_mode(&mut self, mode: ViewMode) {
        self.view_mode = mode;
        self.adjust_current_index_for_visibility();
    }

    /// Returns the name/label of the active view mode.
    pub fn view_mode_name(&self) -> &'static str {
        self.view_mode.name()
    }

    /// Returns the classification of the current image.
    pub fn current_classification(&self) -> Classification {
        if self.queue.images.is_empty() {
            return Classification::Unflagged;
        }
        self.classifications
            .get(self.queue.current_index)
            .cloned()
            .unwrap_or(Classification::Unflagged)
    }

    fn sync_current_adjustments(&mut self) {
        if self.queue.images.is_empty() {
            return;
        }
        let idx = self.queue.current_index;
        if idx < self.adjustments.len() {
            self.adjustments[idx].brightness = self.brightness.value();
            self.adjustments[idx].contrast = self.contrast.value();
        }
    }

    /// Imports image classification states and adjustments from a text file or a JSON manifest.
    pub fn import_classifications(&mut self, import_path: &std::path::Path) -> Result<(), String> {
        let content = std::fs::read_to_string(import_path)
            .map_err(|e| format!("Failed to read import file: {}", e))?;

        let mut imported: HashMap<String, (Classification, ImageAdjustments)> = HashMap::new();

        if import_path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        {
            let parsed: Vec<ClassificationJsonItem> = serde_json::from_str(&content)
                .map_err(|e| format!("Failed to parse JSON manifest: {}", e))?;
            for item in parsed {
                let class = match item.flag.to_lowercase().as_str() {
                    "pick" | "picked" => Classification::Pick,
                    "reject" | "rejected" => Classification::Reject,
                    _ => Classification::Unflagged,
                };
                let adj = ImageAdjustments {
                    brightness: item.brightness.unwrap_or(0),
                    contrast: item.contrast.unwrap_or(0.0),
                    rotation: item.rotation.unwrap_or(0),
                };
                let key = if let Some(ref archive_path) = item.archive {
                    format!("{}::{}", archive_path, item.filename)
                } else {
                    item.filename
                };
                imported.insert(key, (class, adj));
            }
        } else {
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let split_res = line.split_once('\t').or_else(|| line.split_once(':'));
                if let Some((prefix, ident)) = split_res {
                    let ident = ident.trim().to_string();
                    let class = match prefix.trim().to_uppercase().as_str() {
                        "PICK" | "PICKED" => Classification::Pick,
                        "REJECT" | "REJECTED" => Classification::Reject,
                        "UNFLAGGED" => Classification::Unflagged,
                        _ => continue,
                    };
                    imported.insert(ident, (class, ImageAdjustments::default()));
                }
            }
        }

        // Apply imported states to existing files
        for (idx, img) in self.queue.images.iter().enumerate() {
            let ident = img.identifier();
            if let Some(&(class, adj)) = imported.get(&ident) {
                self.classifications[idx] = class;
                self.adjustments[idx] = adj;
            }
        }

        // Adjust selected index for visibility in case files are filtered out
        self.adjust_current_index_for_visibility();

        // Reload current image to apply adjustments immediately
        self.start_load_image();

        Ok(())
    }

    /// Exports image classification states and adjustments to a text file or a JSON manifest.
    pub fn export_classifications(&self, export_path: &std::path::Path) -> Result<(), String> {
        let mut text_lines = Vec::new();
        let mut json_items = Vec::new();

        for (idx, img) in self.queue.images.iter().enumerate() {
            let class = self
                .classifications
                .get(idx)
                .cloned()
                .unwrap_or(Classification::Unflagged);
            let adj = self.adjustments.get(idx).cloned().unwrap_or_default();

            // Only export if image is flagged OR has non-default adjustments
            if class == Classification::Unflagged
                && adj.brightness == 0
                && adj.contrast == 0.0
                && adj.rotation == 0
            {
                continue;
            }

            let ident = img.identifier();

            let (archive, filename) = match img {
                ImageSource::Local(path) => {
                    let abs = if path.is_absolute() {
                        path.clone()
                    } else if let Ok(curr) = std::env::current_dir() {
                        curr.join(path)
                    } else {
                        path.clone()
                    };
                    (None, abs.to_string_lossy().into_owned())
                }
                ImageSource::Cbz {
                    zip_path,
                    file_in_zip,
                } => {
                    let abs_zip = if zip_path.is_absolute() {
                        zip_path.clone()
                    } else if let Ok(curr) = std::env::current_dir() {
                        curr.join(zip_path)
                    } else {
                        zip_path.clone()
                    };
                    (
                        Some(abs_zip.to_string_lossy().into_owned()),
                        file_in_zip.clone(),
                    )
                }
            };

            let flag_str = match class {
                Classification::Pick => "picked",
                Classification::Reject => "rejected",
                Classification::Unflagged => "unflagged",
            };

            let brightness = if adj.brightness != 0 {
                Some(adj.brightness)
            } else {
                None
            };
            let contrast = if adj.contrast != 0.0 {
                Some(adj.contrast)
            } else {
                None
            };
            let rotation = if adj.rotation != 0 {
                Some(adj.rotation)
            } else {
                None
            };

            json_items.push(ClassificationJsonItem {
                archive,
                filename,
                flag: flag_str.to_string(),
                brightness,
                contrast,
                rotation,
            });

            // For text export: only write if flagged
            if class != Classification::Unflagged {
                let text_state = match class {
                    Classification::Pick => "PICK",
                    Classification::Reject => "REJECT",
                    Classification::Unflagged => "UNFLAGGED",
                };
                text_lines.push(format!("{}\t{}", text_state, ident));
            }
        }

        if export_path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        {
            let json_str = serde_json::to_string_pretty(&json_items)
                .map_err(|e| format!("Failed to serialize classifications: {}", e))?;
            std::fs::write(export_path, json_str)
                .map_err(|e| format!("Failed to write export JSON file: {}", e))?;
        } else {
            let content = text_lines.join("\n");
            std::fs::write(export_path, content)
                .map_err(|e| format!("Failed to write export text file: {}", e))?;
        }

        Ok(())
    }

    /// Returns cached filtered files matching the current search query.
    pub fn get_filtered_files(&self) -> &[(usize, String)] {
        &self.filtered_files
    }

    /// Returns cached filtered commands matching the current search query.
    pub fn get_filtered_commands(&self) -> &[PaletteCommand] {
        &self.filtered_commands
    }

    /// Appends a character to the search query and updates matches cache.
    pub fn palette_push_char(&mut self, c: char) {
        self.palette_query.push(c);
        self.palette_selected_index = 0;
        self.update_palette_filter();
    }

    /// Removes the last character from the search query and updates matches cache.
    pub fn palette_pop_char(&mut self) {
        self.palette_query.pop();
        self.palette_selected_index = 0;
        self.update_palette_filter();
    }

    /// Re-calculates and caches the matched files or commands based on the query.
    pub fn update_palette_filter(&mut self) {
        match self.palette_mode {
            PaletteMode::File => {
                self.filtered_files = self.get_filtered_files_uncached();
                if !self.filtered_files.is_empty() {
                    self.palette_selected_index = self
                        .palette_selected_index
                        .min(self.filtered_files.len() - 1);
                } else {
                    self.palette_selected_index = 0;
                }
            }
            PaletteMode::Command => {
                self.filtered_commands = self.get_filtered_commands_uncached();
                if !self.filtered_commands.is_empty() {
                    self.palette_selected_index = self
                        .palette_selected_index
                        .min(self.filtered_commands.len() - 1);
                } else {
                    self.palette_selected_index = 0;
                }
            }
            _ => {}
        }
    }

    fn get_filtered_files_uncached(&mut self) -> Vec<(usize, String)> {
        let query = &self.palette_query;
        if query.is_empty() {
            return self
                .queue
                .display_names
                .iter()
                .enumerate()
                .filter(|&(idx, _)| self.is_visible(idx))
                .map(|(idx, name)| (idx, name.clone()))
                .collect();
        }

        let pattern = nucleo::pattern::Pattern::parse(
            query,
            nucleo::pattern::CaseMatching::Ignore,
            nucleo::pattern::Normalization::Smart,
        );

        #[derive(Clone)]
        struct FileCandidate<'a> {
            index: usize,
            name: &'a str,
        }
        impl<'a> AsRef<str> for FileCandidate<'a> {
            fn as_ref(&self) -> &str {
                self.name
            }
        }

        let candidates: Vec<FileCandidate<'_>> = self
            .queue
            .display_names_lowercase
            .iter()
            .enumerate()
            .filter(|&(index, _)| self.is_visible(index))
            .map(|(index, name)| FileCandidate {
                index,
                name: name.as_str(),
            })
            .collect();

        let mut matches = pattern.match_list(candidates, &mut self.matcher);
        matches.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.index.cmp(&b.0.index)));

        matches
            .into_iter()
            .map(|(candidate, _score)| {
                (
                    candidate.index,
                    self.queue.display_names[candidate.index].clone(),
                )
            })
            .collect()
    }

    fn get_filtered_commands_uncached(&mut self) -> Vec<PaletteCommand> {
        let query = &self.palette_query;
        if query.is_empty() {
            return get_commands()
                .iter()
                .filter(|cmd| cmd.item.show_in_palette)
                .cloned()
                .collect();
        }

        let pattern = nucleo::pattern::Pattern::parse(
            query,
            nucleo::pattern::CaseMatching::Ignore,
            nucleo::pattern::Normalization::Smart,
        );

        #[derive(Clone)]
        struct CmdCandidate {
            cmd: PaletteCommand,
        }
        impl AsRef<str> for CmdCandidate {
            fn as_ref(&self) -> &str {
                &self.cmd.search_text
            }
        }

        let candidates: Vec<CmdCandidate> = get_commands()
            .iter()
            .filter(|cmd| cmd.item.show_in_palette)
            .map(|cmd| CmdCandidate { cmd: cmd.clone() })
            .collect();

        let mut matches = pattern.match_list(candidates, &mut self.matcher);
        matches.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| (a.0.cmd.cmd as usize).cmp(&(b.0.cmd.cmd as usize)))
        });

        matches
            .into_iter()
            .map(|(candidate, _score)| candidate.cmd)
            .collect()
    }

    pub fn filter_name(&self) -> &'static str {
        match self.filter_type {
            FilterType::Nearest => "Nearest",
            FilterType::Triangle => "Linear",
            FilterType::CatmullRom => "Cubic",
            FilterType::Mitchell => "Mitchell",
            FilterType::Gaussian => "Gaussian",
            FilterType::Lanczos3 => "Lanczos",
            FilterType::Hamming => "Hamming",
        }
    }

    pub fn cycle_filter(&mut self) {
        if self.is_loading {
            return;
        }
        self.filter_type = match self.filter_type {
            FilterType::Nearest => FilterType::Hamming,
            FilterType::Hamming => FilterType::Triangle,
            FilterType::Triangle => FilterType::CatmullRom,
            FilterType::CatmullRom => FilterType::Mitchell,
            FilterType::Mitchell => FilterType::Gaussian,
            FilterType::Gaussian => FilterType::Lanczos3,
            FilterType::Lanczos3 => FilterType::Nearest,
        };
        self.needs_update = true;
    }

    pub fn execute_command(&mut self, cmd: Command) {
        match cmd {
            Command::ResetView => self.reset_view(),
            Command::ActualSize => self.set_actual_size(),
            Command::RotateClockwise => self.rotate_clockwise(),
            Command::RotateCounterClockwise => self.rotate_counter_clockwise(),
            Command::NextImage => self.next_image(),
            Command::PreviousImage => self.prev_image(),
            Command::ZoomIn => self.zoom_in(),
            Command::ZoomOut => self.zoom_out(),
            Command::Quit => self.running = false,
            Command::SetFilterNearest => {
                self.filter_type = FilterType::Nearest;
                self.needs_update = true;
            }
            Command::SetFilterLinear => {
                self.filter_type = FilterType::Triangle;
                self.needs_update = true;
            }
            Command::SetFilterCubic => {
                self.filter_type = FilterType::CatmullRom;
                self.needs_update = true;
            }
            Command::SetFilterMitchell => {
                self.filter_type = FilterType::Mitchell;
                self.needs_update = true;
            }
            Command::SetFilterGaussian => {
                self.filter_type = FilterType::Gaussian;
                self.needs_update = true;
            }
            Command::SetFilterLanczos => {
                self.filter_type = FilterType::Lanczos3;
                self.needs_update = true;
            }
            Command::SetFilterHamming => {
                self.filter_type = FilterType::Hamming;
                self.needs_update = true;
            }
            Command::NextFilter => self.cycle_filter(),
            Command::GoToImage => self.open_prompt(PromptType::GoToImage),
            Command::SetBrightness => self.open_prompt(PromptType::SetBrightness),
            Command::SetContrast => self.open_prompt(PromptType::SetContrast),
            Command::SetScaleNone => {
                self.scale_mode = ScaleMode::None;
                self.apply_scale_mode();
            }
            Command::SetScaleShrink => {
                self.scale_mode = ScaleMode::Shrink;
                self.apply_scale_mode();
            }
            Command::SetScaleFit => {
                self.scale_mode = ScaleMode::Full;
                self.apply_scale_mode();
            }
            Command::SetScaleCrop => {
                self.scale_mode = ScaleMode::Crop;
                self.apply_scale_mode();
            }
            Command::CycleScaleMode => self.cycle_scale_mode(),
            Command::IncreaseBrightness => self.increase_brightness(),
            Command::DecreaseBrightness => self.decrease_brightness(),
            Command::IncreaseContrast => self.increase_contrast(),
            Command::DecreaseContrast => self.decrease_contrast(),
            Command::PredefinedZoomIn => self.jump_zoom_in(),
            Command::PredefinedZoomOut => self.jump_zoom_out(),
            Command::PanLeft => self.pan_left(),
            Command::PanRight => self.pan_right(),
            Command::PanUp => self.pan_up(),
            Command::PanDown => self.pan_down(),
            Command::CommandPalette => self.open_palette(PaletteMode::Command),
            Command::FileSearch => self.open_palette(PaletteMode::File),
            Command::SlideshowIncrease => {
                let current_sec = self.slideshow_config.seconds();
                self.slideshow_config = SlideshowConfig::new(current_sec.saturating_add(1).max(1));
                self.slideshow_last_transition = std::time::Instant::now();
            }
            Command::SlideshowDecrease => {
                let current_sec = self.slideshow_config.seconds();
                self.slideshow_config = SlideshowConfig::new(current_sec.saturating_sub(1));
                self.slideshow_last_transition = std::time::Instant::now();
            }
            Command::SetSlideshow => self.open_prompt(PromptType::SetSlideshow),
            Command::ToggleThumbnail => {
                if self.thumbnail_image.is_some() {
                    self.show_thumbnail_only = !self.show_thumbnail_only;
                    self.needs_update = true;
                    self.needs_clear_once = true;
                }
            }
            Command::ShowInfo => {
                if self.palette_mode == PaletteMode::Info {
                    if self.last_info_toggle.is_none()
                        || self.last_info_toggle.unwrap().elapsed()
                            > std::time::Duration::from_millis(200)
                    {
                        self.palette_mode = PaletteMode::Closed;
                        self.needs_clear_once = true;
                        self.last_info_toggle = Some(std::time::Instant::now());
                    }
                } else {
                    self.palette_mode = PaletteMode::Info;
                    self.palette_height = 19;
                    self.needs_clear_once = true;
                    self.last_info_toggle = Some(std::time::Instant::now());
                }
            }
            Command::MarkPick => self.mark_pick(),
            Command::MarkReject => self.mark_reject(),
            Command::Unflag => self.unflag_image(),
            Command::CycleView => self.cycle_view_mode(),
            Command::SetViewDefault => self.set_view_mode(ViewMode::Default),
            Command::SetViewUnflagged => self.set_view_mode(ViewMode::Unflagged),
            Command::SetViewPicks => self.set_view_mode(ViewMode::Picks),
            Command::SetViewRejects => self.set_view_mode(ViewMode::Rejects),
            Command::SetViewAll => self.set_view_mode(ViewMode::All),
        }
    }

    pub fn open_palette(&mut self, mode: PaletteMode) {
        self.palette_mode = mode;
        self.palette_query.clear();
        self.palette_selected_index = match mode {
            PaletteMode::File => self.get_visible_position().unwrap_or(0),
            PaletteMode::Command => 0,
            _ => 0,
        };
        self.needs_clear = true;
        self.update_palette_filter();

        let max_text_width = match mode {
            PaletteMode::File => self
                .queue
                .display_names
                .iter()
                .map(|name| name.len())
                .max()
                .unwrap_or(0) as u16,
            PaletteMode::Command => get_commands()
                .iter()
                .filter(|cmd| cmd.item.show_in_palette)
                .map(|cmd| cmd.item.name.len() + 3 + cmd.item.description.len())
                .max()
                .unwrap_or(0) as u16,
            _ => 0,
        };

        self.palette_width = max_text_width + 5;
        self.palette_height = 0;
    }

    pub fn open_prompt(&mut self, prompt_type: PromptType) {
        self.palette_mode = PaletteMode::Prompt;
        self.prompt_type = Some(prompt_type);
        self.palette_query.clear();
        self.palette_selected_index = 0;
        self.palette_width = 45;
        self.palette_height = 0;
        self.needs_clear = true;
    }

    pub fn execute_prompt(&mut self, prompt_type: PromptType) {
        (|| match prompt_type {
            PromptType::GoToImage => {
                if self.queue.is_empty() {
                    return;
                }
                let visible: Vec<usize> = (0..self.queue.images.len())
                    .filter(|&idx| self.is_visible(idx))
                    .collect();
                if visible.is_empty() {
                    return;
                }
                let input = self.palette_query.trim();
                let Ok(adj) = input.parse::<Adjustment<usize>>() else {
                    return;
                };
                let current_visible_pos = visible
                    .iter()
                    .position(|&idx| idx == self.queue.current_index)
                    .unwrap_or(0);
                let mut new_visible_pos = current_visible_pos;
                match adj {
                    Adjustment::Absolute(val) => {
                        if let Some(val_minus_1) = val.checked_sub(1) {
                            new_visible_pos = val_minus_1.min(visible.len() - 1);
                        }
                    }
                    Adjustment::RelativeAdd(val) => {
                        new_visible_pos = (current_visible_pos + val).min(visible.len() - 1);
                    }
                    Adjustment::RelativeSub(val) => {
                        new_visible_pos = current_visible_pos.saturating_sub(val);
                    }
                }
                let new_idx = visible[new_visible_pos];
                if new_idx != self.queue.current_index {
                    self.queue.current_index = new_idx;
                    self.start_load_image();
                }
            }
            PromptType::SetBrightness => {
                if self.original_image.is_none() {
                    return;
                }
                let input = self.palette_query.trim();
                let Ok(adj) = input.parse::<Adjustment<i32>>() else {
                    return;
                };
                let old = self.brightness;
                match adj {
                    Adjustment::Absolute(val) => self.brightness = Brightness::new(val),
                    Adjustment::RelativeAdd(val) => self.brightness.adjust(val),
                    Adjustment::RelativeSub(val) => self.brightness.adjust(-val),
                }
                if old != self.brightness {
                    self.sync_current_adjustments();
                    self.needs_update = true;
                }
            }
            PromptType::SetContrast => {
                if self.original_image.is_none() {
                    return;
                }
                let input = self.palette_query.trim();
                let Ok(adj) = input.parse::<Adjustment<f32>>() else {
                    return;
                };
                let mut next = self.contrast;
                match adj {
                    Adjustment::Absolute(val) => next = Contrast::new(val),
                    Adjustment::RelativeAdd(val) => next.adjust(val),
                    Adjustment::RelativeSub(val) => next.adjust(-val),
                }
                if self.contrast.update(next.value()) {
                    self.sync_current_adjustments();
                    self.needs_update = true;
                }
            }
            PromptType::SetSlideshow => {
                let input = self.palette_query.trim();
                let Ok(adj) = input.parse::<Adjustment<u32>>() else {
                    return;
                };
                let mut new_val = self.slideshow_config.seconds();
                match adj {
                    Adjustment::Absolute(val) => new_val = val,
                    Adjustment::RelativeAdd(val) => new_val = new_val.saturating_add(val),
                    Adjustment::RelativeSub(val) => new_val = new_val.saturating_sub(val),
                }
                if new_val != self.slideshow_config.seconds() {
                    self.slideshow_config = SlideshowConfig::new(new_val);
                    self.slideshow_last_transition = std::time::Instant::now();
                }
            }
        })();
        self.palette_mode = PaletteMode::Closed;
        self.prompt_type = None;
        self.needs_clear_once = true;
    }

    pub fn get_sliding_window_indices(&self) -> Vec<usize> {
        let n = 2; // Cache size N=2 (caches current + 2 before + 2 after)
        let visible: Vec<usize> = (0..self.queue.images.len())
            .filter(|&idx| self.is_visible(idx))
            .collect();
        let total = visible.len();
        if total == 0 {
            return Vec::new();
        }
        let current_pos = visible
            .iter()
            .position(|&idx| idx == self.queue.current_index);
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

    pub fn trigger_prefetch(&self) {
        if self.queue.images.len() <= 1 {
            return;
        }

        let window_indices = self.get_sliding_window_indices();
        let mut to_prefetch = Vec::new();

        {
            let mut cache = self.prefetch_cache.lock().unwrap();
            cache.retain(|idx, _| window_indices.contains(idx));
            for idx in window_indices {
                if idx == self.queue.current_index {
                    continue;
                }
                if !cache.contains_key(&idx) {
                    to_prefetch.push(idx);
                }
            }
        }

        for idx in to_prefetch {
            let source = self.queue.images[idx].clone();
            let _ = self.loader_tx.send(LoaderRequest {
                idx,
                source,
                is_prefetch: true,
                sequence: self.current_sequence,
            });
        }
    }

    pub fn start_load_image(&mut self) {
        if self.queue.is_empty() {
            self.original_image = None;
            self.image_protocol = None;
            self.error_message = Some("No supported images found".to_string());
            return;
        }

        self.error_message = None;
        self.clear_on_protocol_receive = true;
        self.slideshow_last_transition = std::time::Instant::now();

        let adj = self
            .adjustments
            .get(self.queue.current_index)
            .copied()
            .unwrap_or_default();
        self.brightness = Brightness::new(adj.brightness);
        self.contrast = Contrast::new(adj.contrast);

        // Check if the image is in the prefetch cache
        let cached = {
            let mut cache = self.prefetch_cache.lock().unwrap();
            if cache
                .get(&self.queue.current_index)
                .is_some_and(|c| c.image.is_some())
            {
                cache.remove(&self.queue.current_index)
            } else {
                None
            }
        };

        if let Some(cached_img) = cached {
            self.current_sequence += 1;
            self.original_image = cached_img.image.map(|img| {
                if adj.rotation != 0 {
                    let rotated = adj.rotate_image(&img);
                    self.img_width = rotated.width();
                    self.img_height = rotated.height();
                    Arc::new(rotated)
                } else {
                    self.img_width = cached_img.width;
                    self.img_height = cached_img.height;
                    img
                }
            });

            self.thumbnail_image = cached_img.thumbnail.map(|thumb| {
                if adj.rotation != 0 {
                    Arc::new(adj.rotate_image(&thumb))
                } else {
                    thumb
                }
            });
            self.show_thumbnail_only = false;

            if let Some(ref thumb) = self.thumbnail_image {
                self.stats.thumbnail_load_duration = Some(cached_img.thumbnail_decode_duration);
                self.stats.thumbnail_dimensions = Some((thumb.width(), thumb.height()));
            } else {
                self.stats.thumbnail_load_duration = None;
                self.stats.thumbnail_dimensions = None;
            }

            self.current_icon = cached_img.icon;
            self.zoom_factor = 1.0;
            self.pan_offset = PanOffset::ZERO;
            self.is_loading = false;
            self.needs_update = true;
            self.zoom_needs_initialization = true;

            // Set stats for cache hit
            self.stats.load_duration = cached_img.decode_duration;
            self.stats.is_prefetch_cache_hit = true;
            self.stats.disk_size = cached_img.disk_size;

            self.trigger_prefetch();
            return;
        }

        // Cache miss: load as normal via background loader worker
        self.original_image = None;
        self.thumbnail_image = None;
        self.show_thumbnail_only = false;
        self.stats.thumbnail_load_duration = None;
        self.stats.thumbnail_dimensions = None;
        self.image_protocol = None;
        self.is_loading = true;
        self.loading_start_time = Some(Instant::now());
        self.current_sequence += 1;

        self.zoom_factor = 1.0;
        self.pan_offset = PanOffset::ZERO;
        self.zoom_needs_initialization = true;

        let source = self.queue.images[self.queue.current_index].clone();
        let _ = self.loader_tx.send(LoaderRequest {
            idx: self.queue.current_index,
            source,
            is_prefetch: false,
            sequence: self.current_sequence,
        });

        // Trigger prefetching immediately under this new sequence
        self.trigger_prefetch();
    }

    pub fn update_channels(&mut self) {
        while let Ok(resp) = self.response_rx.try_recv() {
            if resp.sequence < self.current_sequence && !resp.is_prefetch {
                continue;
            }

            match resp.result {
                Ok((img, w, h, icon, disk_size)) => {
                    let shared_img = Arc::new(img);
                    if resp.is_prefetch {
                        let window_indices = self.get_sliding_window_indices();
                        if window_indices.contains(&resp.idx) {
                            let mut cache = self.prefetch_cache.lock().unwrap();
                            if resp.is_thumbnail {
                                if let Some(cached) = cache.get_mut(&resp.idx) {
                                    cached.thumbnail = Some(shared_img);
                                    cached.thumbnail_decode_duration = resp.decode_duration;
                                } else {
                                    cache.insert(
                                        resp.idx,
                                        CachedImage {
                                            image: None,
                                            thumbnail: Some(shared_img),
                                            width: w,
                                            height: h,
                                            icon,
                                            decode_duration: std::time::Duration::ZERO,
                                            thumbnail_decode_duration: resp.decode_duration,
                                            disk_size,
                                        },
                                    );
                                }
                            } else {
                                if let Some(cached) = cache.get_mut(&resp.idx) {
                                    cached.image = Some(shared_img);
                                    cached.width = w;
                                    cached.height = h;
                                    cached.icon = icon;
                                    cached.decode_duration = resp.decode_duration;
                                    cached.disk_size = disk_size;
                                } else {
                                    cache.insert(
                                        resp.idx,
                                        CachedImage {
                                            image: Some(shared_img),
                                            thumbnail: None,
                                            width: w,
                                            height: h,
                                            icon,
                                            decode_duration: resp.decode_duration,
                                            thumbnail_decode_duration: std::time::Duration::ZERO,
                                            disk_size,
                                        },
                                    );
                                }
                            }
                        }
                    } else if resp.idx == self.queue.current_index {
                        let adj = self.adjustments.get(resp.idx).copied().unwrap_or_default();
                        let rotated_img = if adj.rotation != 0 {
                            Arc::new(adj.rotate_image(&shared_img))
                        } else {
                            shared_img
                        };

                        if resp.is_thumbnail {
                            self.img_width = rotated_img.width();
                            self.img_height = rotated_img.height();
                            self.current_icon = icon;
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
                        } else {
                            self.img_width = rotated_img.width();
                            self.img_height = rotated_img.height();
                            self.original_image = Some(rotated_img);
                            self.current_icon = icon;
                            self.error_message = None;
                            self.is_loading = false;
                            self.needs_update = true;

                            self.stats.load_duration = resp.decode_duration;
                            self.stats.is_prefetch_cache_hit = false;
                            self.stats.disk_size = disk_size;

                            self.trigger_prefetch();
                        }
                    }
                }
                Err(err) => {
                    if !resp.is_prefetch && resp.idx == self.queue.current_index {
                        self.original_image = None;
                        self.image_protocol = None;
                        self.error_message = Some(err);
                        self.is_loading = false;
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
            self.image_protocol = Some(resp.protocol);
            self.rendered_size_cells = resp.rendered_cells;
            self.is_loading = false;
            if self.clear_on_protocol_receive {
                self.clear_on_protocol_receive = false;
                self.needs_clear = true;
            }
            if self.palette_mode != PaletteMode::Closed {
                self.needs_clear_once = true;
            }
            self.stats.process_duration = resp.process_duration;
            self.stats.protocol_duration = resp.protocol_duration;
            self.stats.protocol_width = resp.target_width;
            self.stats.protocol_height = resp.target_height;
        }
    }

    pub fn update_layout(&mut self, term_height: u16) {
        if self.palette_mode == PaletteMode::Closed {
            return;
        }

        let new_h = match self.palette_mode {
            PaletteMode::Prompt => 4.min(term_height.saturating_sub(1)),
            PaletteMode::Info => 19.min(term_height.saturating_sub(1)),
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
            self.needs_clear_once = true;
        }
    }

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

        let active_img = if self.show_thumbnail_only && self.thumbnail_image.is_some() {
            self.thumbnail_image.as_ref()
        } else {
            self.original_image.as_ref()
        };

        if let Some(img) = active_img {
            let w_orig = self.img_width as f64;
            let h_orig = self.img_height as f64;

            // 1. Calculate fit-to-screen scale 's'
            let s_w = widget_w_px / w_orig;
            let s_h = widget_h_px / h_orig;
            let s = s_w.min(s_h);

            if self.zoom_needs_initialization && s > 0.0 {
                self.zoom_needs_initialization = false;
                self.zoom_factor = match self.scale_mode {
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
                };
                self.pan_offset = PanOffset::ZERO;
            }

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
            let rendered_cells = (cells_w.clamp(1, widget_w), cells_h.clamp(1, widget_h));

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
                brightness: self.brightness,
                contrast: self.contrast,
                rendered_size_cells: rendered_cells,
                sequence: self.current_sequence,
            };

            let _ = self.resize_tx.send(req);
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

    pub fn should_clear_on_update(&self) -> bool {
        matches!(self.picker.protocol_type(), ProtocolType::Sixel)
    }

    pub fn zoom_in(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        let s = self.get_fit_scale();
        if s > 0.0 {
            self.zoom_factor = (self.zoom_factor * 1.25).min(102.4 / s);
            self.clamp_pan();
            self.needs_update = true;
        }
    }

    pub fn zoom_out(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        let s = self.get_fit_scale();
        if s > 0.0 {
            self.zoom_factor = (self.zoom_factor / 1.25).max(0.01 / s);
            self.clamp_pan();
            self.needs_update = true;
        }
    }

    pub fn jump_zoom_in(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        let fit_scale = self.get_fit_scale();
        if fit_scale <= 0.0 {
            return;
        }

        // Calculate crop scale (crop to fill)
        let (widget_w_cells, widget_h_cells) = self.last_widget_size;
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
        let crop_scale = s_w.max(s_h);

        // Predefined levels in terms of absolute scale (scale = fit_scale * zoom_factor)
        let shrink_to_fit_scale = fit_scale.min(1.0);
        let fit_view_scale = fit_scale;
        let crop_to_fill_scale = crop_scale;
        let one_to_one_scale = 1.0;
        let two_to_one_scale = 2.0;
        let four_to_one_scale = 4.0;

        let mut levels = vec![
            shrink_to_fit_scale,
            fit_view_scale,
            crop_to_fill_scale,
            one_to_one_scale,
            two_to_one_scale,
            four_to_one_scale,
        ];
        levels.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        levels.dedup_by(|a, b| (*a - *b).abs() < 0.01);

        let current_scale = fit_scale * self.zoom_factor;

        let mut target_scale = None;
        for &lvl in &levels {
            if lvl > current_scale + 0.01 {
                target_scale = Some(lvl);
                break;
            }
        }

        if let Some(target) = target_scale {
            self.zoom_factor = target / fit_scale;
        } else {
            // Double the scale if already past maximum level
            self.zoom_factor = (current_scale * 2.0).min(102.4) / fit_scale;
        }

        self.clamp_pan();
        self.needs_update = true;
    }

    pub fn jump_zoom_out(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        let fit_scale = self.get_fit_scale();
        if fit_scale <= 0.0 {
            return;
        }

        // Calculate crop scale (crop to fill)
        let (widget_w_cells, widget_h_cells) = self.last_widget_size;
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
        let crop_scale = s_w.max(s_h);

        // Predefined levels in terms of absolute scale (scale = fit_scale * zoom_factor)
        let shrink_to_fit_scale = fit_scale.min(1.0);
        let fit_view_scale = fit_scale;
        let crop_to_fill_scale = crop_scale;
        let one_to_one_scale = 1.0;
        let two_to_one_scale = 2.0;
        let four_to_one_scale = 4.0;

        let mut levels = vec![
            shrink_to_fit_scale,
            fit_view_scale,
            crop_to_fill_scale,
            one_to_one_scale,
            two_to_one_scale,
            four_to_one_scale,
        ];
        levels.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        levels.dedup_by(|a, b| (*a - *b).abs() < 0.01);

        let current_scale = fit_scale * self.zoom_factor;

        let mut target_scale = None;
        for &lvl in levels.iter().rev() {
            if lvl < current_scale - 0.01 {
                target_scale = Some(lvl);
                break;
            }
        }

        if let Some(target) = target_scale {
            self.zoom_factor = target / fit_scale;
        } else {
            // Halve the scale if already below minimum level
            self.zoom_factor = (current_scale / 2.0).max(0.01) / fit_scale;
        }

        self.clamp_pan();
        self.needs_update = true;
    }

    pub fn set_actual_size(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        let s = self.get_fit_scale();
        if s > 0.0 {
            self.zoom_factor = 1.0 / s;
            self.clamp_pan();
            self.needs_update = true;
            self.clear_on_protocol_receive = true;
        }
    }

    pub fn reset_view(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        self.apply_scale_mode();
        self.brightness = Brightness::ZERO;
        self.contrast = Contrast::ZERO;
        self.sync_current_adjustments();
        self.clear_on_protocol_receive = true;
    }

    pub fn apply_scale_mode(&mut self) {
        let s = self.get_fit_scale();
        if s > 0.0 {
            self.zoom_factor = match self.scale_mode {
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
                    let (widget_w_cells, widget_h_cells) = self.last_widget_size;
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
                    s_w.max(s_h) / s
                }
            };
            self.pan_offset = PanOffset::ZERO;
            self.needs_update = true;
        }
    }

    pub fn cycle_scale_mode(&mut self) {
        if self.is_loading {
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

    pub fn increase_brightness(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        let old = self.brightness;
        self.brightness.adjust(10);
        if old != self.brightness {
            self.sync_current_adjustments();
            self.needs_update = true;
        }
    }

    pub fn decrease_brightness(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        let old = self.brightness;
        self.brightness.adjust(-10);
        if old != self.brightness {
            self.sync_current_adjustments();
            self.needs_update = true;
        }
    }

    pub fn increase_contrast(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        let mut next = self.contrast;
        next.adjust(10.0);
        if self.contrast.update(next.value()) {
            self.sync_current_adjustments();
            self.needs_update = true;
        }
    }

    pub fn decrease_contrast(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        let mut next = self.contrast;
        next.adjust(-10.0);
        if self.contrast.update(next.value()) {
            self.sync_current_adjustments();
            self.needs_update = true;
        }
    }

    pub fn clamp_pan(&mut self) {
        if self.original_image.is_none() {
            return;
        }
        self.pan_offset.clamp(self.img_width, self.img_height);
    }

    pub fn pan_left(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        let step = self.pan_step_x();
        self.pan_offset.x -= step;
        self.clamp_pan();
        self.needs_update = true;
    }

    pub fn pan_right(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        let step = self.pan_step_x();
        self.pan_offset.x += step;
        self.clamp_pan();
        self.needs_update = true;
    }

    pub fn pan_up(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        let step = self.pan_step_y();
        self.pan_offset.y -= step;
        self.clamp_pan();
        self.needs_update = true;
    }

    pub fn pan_down(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        let step = self.pan_step_y();
        self.pan_offset.y += step;
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

    pub fn rotate_clockwise(&mut self) {
        if self.is_loading {
            return;
        }
        if let Some(img) = self.original_image.take() {
            let rotated = img.rotate90();
            self.img_width = rotated.width();
            self.img_height = rotated.height();
            self.original_image = Some(Arc::new(rotated));

            if let Some(thumb) = self.thumbnail_image.take() {
                self.thumbnail_image = Some(Arc::new(thumb.rotate90()));
            }

            let idx = self.queue.current_index;
            if idx < self.adjustments.len() {
                self.adjustments[idx].rotation = (self.adjustments[idx].rotation + 90) % 360;
            }

            self.zoom_factor = 1.0;
            self.pan_offset = PanOffset::ZERO;
            self.needs_update = true;
            self.clear_on_protocol_receive = true;
        }
    }

    pub fn rotate_counter_clockwise(&mut self) {
        if self.is_loading {
            return;
        }
        if let Some(img) = self.original_image.take() {
            let rotated = img.rotate270();
            self.img_width = rotated.width();
            self.img_height = rotated.height();
            self.original_image = Some(Arc::new(rotated));

            if let Some(thumb) = self.thumbnail_image.take() {
                self.thumbnail_image = Some(Arc::new(thumb.rotate270()));
            }

            let idx = self.queue.current_index;
            if idx < self.adjustments.len() {
                self.adjustments[idx].rotation = (self.adjustments[idx].rotation + 270) % 360;
            }

            self.zoom_factor = 1.0;
            self.pan_offset = PanOffset::ZERO;
            self.needs_update = true;
            self.clear_on_protocol_receive = true;
        }
    }

    pub fn current_filename(&self) -> &str {
        if self.queue.is_empty() || self.get_visible_count() == 0 {
            return "No file loaded";
        }
        self.queue.get_current_filename()
    }

    pub fn next_image(&mut self) {
        let total = self.queue.images.len();
        if total <= 1 {
            return;
        }
        let start = self.queue.current_index;
        let mut idx = start;
        loop {
            idx = (idx + 1) % total;
            if self.is_visible(idx) {
                if idx != start {
                    self.queue.current_index = idx;
                    self.start_load_image();
                }
                break;
            }
            if idx == start {
                break;
            }
        }
    }

    pub fn prev_image(&mut self) {
        let total = self.queue.images.len();
        if total <= 1 {
            return;
        }
        let start = self.queue.current_index;
        let mut idx = start;
        loop {
            if idx == 0 {
                idx = total - 1;
            } else {
                idx -= 1;
            }
            if self.is_visible(idx) {
                if idx != start {
                    self.queue.current_index = idx;
                    self.start_load_image();
                }
                break;
            }
            if idx == start {
                break;
            }
        }
    }

    /// Handles a Crossterm input event (keyboard or mouse).
    pub fn handle_event(&mut self, ev: Event, terminal_height: u16) {
        if let (
            PaletteMode::Info,
            Event::Key(crossterm::event::KeyEvent {
                code,
                kind: KeyEventKind::Press,
                ..
            }),
        ) = (self.palette_mode, &ev)
        {
            match code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.palette_mode = PaletteMode::Closed;
                    self.needs_clear_once = true;
                    return;
                }
                KeyCode::Char('d') => {
                    if self.last_info_toggle.is_none()
                        || self.last_info_toggle.unwrap().elapsed()
                            > std::time::Duration::from_millis(200)
                    {
                        self.palette_mode = PaletteMode::Closed;
                        self.needs_clear_once = true;
                        self.last_info_toggle = Some(std::time::Instant::now());
                    }
                    return;
                }
                _ => {}
            }
        }

        if self.palette_mode != PaletteMode::Closed && self.palette_mode != PaletteMode::Info {
            if let Event::Key(key) = ev {
                if key.kind != KeyEventKind::Press {
                    return;
                }
                match key.code {
                    KeyCode::Esc => {
                        self.palette_mode = PaletteMode::Closed;
                        self.needs_update = true;
                        self.needs_clear_once = true;
                    }
                    KeyCode::Enter => match self.palette_mode {
                        PaletteMode::File => {
                            let files = self.get_filtered_files();
                            if !files.is_empty() && self.palette_selected_index < files.len() {
                                self.queue.current_index = files[self.palette_selected_index].0;
                                self.start_load_image();
                            }
                            self.palette_mode = PaletteMode::Closed;
                            self.needs_update = true;
                            self.needs_clear_once = true;
                        }
                        PaletteMode::Command => {
                            let cmds = self.get_filtered_commands();
                            if !cmds.is_empty() && self.palette_selected_index < cmds.len() {
                                let cmd = cmds[self.palette_selected_index].cmd;
                                self.execute_command(cmd);
                            }
                            if self.palette_mode == PaletteMode::Command {
                                self.palette_mode = PaletteMode::Closed;
                                self.needs_update = true;
                                self.needs_clear_once = true;
                            }
                        }
                        PaletteMode::Prompt => {
                            if let Some(prompt_type) = self.prompt_type {
                                self.execute_prompt(prompt_type);
                            }
                        }
                        _ => {}
                    },
                    KeyCode::Up if self.palette_selected_index > 0 => {
                        self.palette_selected_index -= 1;
                    }
                    KeyCode::Down => {
                        let max_len = match self.palette_mode {
                            PaletteMode::File => self.get_filtered_files().len(),
                            PaletteMode::Command => self.get_filtered_commands().len(),
                            _ => 0,
                        };
                        if max_len > 0 && self.palette_selected_index < max_len - 1 {
                            self.palette_selected_index += 1;
                        }
                    }
                    KeyCode::PageUp => {
                        let max_len = match self.palette_mode {
                            PaletteMode::File => self.get_filtered_files().len(),
                            PaletteMode::Command => self.get_filtered_commands().len(),
                            _ => 0,
                        };
                        let viewport_h = terminal_height.saturating_sub(1);
                        let max_h = (viewport_h as f64 * 0.5).round() as u16;
                        let palette_h = (max_len as u16 + 4).max(12).min(max_h);
                        let page_size = (palette_h as usize).saturating_sub(4);

                        self.palette_selected_index =
                            self.palette_selected_index.saturating_sub(page_size);
                    }
                    KeyCode::PageDown => {
                        let max_len = match self.palette_mode {
                            PaletteMode::File => self.get_filtered_files().len(),
                            PaletteMode::Command => self.get_filtered_commands().len(),
                            _ => 0,
                        };
                        if max_len > 0 {
                            let viewport_h = terminal_height.saturating_sub(1);
                            let max_h = (viewport_h as f64 * 0.5).round() as u16;
                            let palette_h = (max_len as u16 + 4).max(12).min(max_h);
                            let page_size = (palette_h as usize).saturating_sub(4);

                            self.palette_selected_index =
                                (self.palette_selected_index + page_size).min(max_len - 1);
                        }
                    }
                    KeyCode::Char('k')
                        if key
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::CONTROL)
                            && self.palette_selected_index > 0 =>
                    {
                        self.palette_selected_index -= 1;
                    }
                    KeyCode::Char('j')
                        if key
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::CONTROL) =>
                    {
                        let max_len = match self.palette_mode {
                            PaletteMode::File => self.get_filtered_files().len(),
                            PaletteMode::Command => self.get_filtered_commands().len(),
                            _ => 0,
                        };
                        if max_len > 0 && self.palette_selected_index < max_len - 1 {
                            self.palette_selected_index += 1;
                        }
                    }
                    KeyCode::Backspace => {
                        self.palette_pop_char();
                    }
                    KeyCode::Char(c) => {
                        self.palette_push_char(c);
                    }
                    _ => {}
                }
            }
        } else {
            if let Some(cmd) = Command::from_event(&ev) {
                self.execute_command(cmd);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_worker::ImageSource;
    use ratatui_image::picker::Picker;
    use std::path::PathBuf;

    #[test]
    fn test_import_export_text_and_json() {
        let _ = std::fs::create_dir_all("target/tmp");
        let json_path = PathBuf::from("target/tmp/test_classifications.json");
        let txt_path = PathBuf::from("target/tmp/test_classifications.txt");

        let images = vec![
            ImageSource::Local(PathBuf::from("img1.png")),
            ImageSource::Local(PathBuf::from("img2.png")),
            ImageSource::Cbz {
                zip_path: PathBuf::from("archive.cbz"),
                file_in_zip: "page1.png".to_string(),
            },
        ];

        let picker = Picker::halfblocks();
        let mut app = App::new(
            images.clone(),
            0,
            picker,
            crate::image_worker::FilterType::Nearest,
            crate::image_worker::ScaleMode::Shrink,
            true,
        )
        .unwrap();

        app.classifications[0] = Classification::Pick;
        app.classifications[1] = Classification::Reject;
        app.classifications[2] = Classification::Pick;

        app.adjustments[0] = ImageAdjustments {
            brightness: 20,
            contrast: 15.0,
            rotation: 90,
        };
        app.adjustments[2] = ImageAdjustments {
            brightness: -30,
            contrast: 0.0,
            rotation: 180,
        };

        // Test Export to JSON
        app.export_classifications(&json_path).unwrap();
        assert!(json_path.exists());
        let json_content = std::fs::read_to_string(&json_path).unwrap();
        // Check that it's serialized as a JSON array of objects
        let parsed_json: Vec<ClassificationJsonItem> = serde_json::from_str(&json_content).unwrap();
        assert_eq!(parsed_json.len(), 3);
        assert_eq!(parsed_json[0].flag, "picked");
        assert_eq!(parsed_json[0].archive, None);
        assert_eq!(parsed_json[0].brightness, Some(20));
        assert_eq!(parsed_json[0].contrast, Some(15.0));
        assert_eq!(parsed_json[0].rotation, Some(90));
        assert_eq!(parsed_json[1].flag, "rejected");
        assert_eq!(parsed_json[1].archive, None);
        assert_eq!(parsed_json[1].brightness, None);
        assert_eq!(parsed_json[1].contrast, None);
        assert_eq!(parsed_json[1].rotation, None);
        assert_eq!(parsed_json[2].flag, "picked");
        assert!(parsed_json[2].archive.is_some());
        assert_eq!(parsed_json[2].filename, "page1.png");
        assert_eq!(parsed_json[2].brightness, Some(-30));
        assert_eq!(parsed_json[2].contrast, None);
        assert_eq!(parsed_json[2].rotation, Some(180));

        // Test Export to Text
        app.export_classifications(&txt_path).unwrap();
        assert!(txt_path.exists());
        let txt_content = std::fs::read_to_string(&txt_path).unwrap();
        // Verify that the tab character is used to separate
        assert!(txt_content.contains("PICK\t"));
        assert!(txt_content.contains("REJECT\t"));
        // Text format does not contain adjustment parameters
        assert!(!txt_content.contains("20"));
        assert!(!txt_content.contains("15"));

        // Clear classifications and adjustments in app
        app.classifications = vec![Classification::Unflagged; 3];
        app.adjustments = vec![ImageAdjustments::default(); 3];

        // Import from JSON
        app.import_classifications(&json_path).unwrap();
        assert_eq!(app.classifications[0], Classification::Pick);
        assert_eq!(app.classifications[1], Classification::Reject);
        assert_eq!(app.classifications[2], Classification::Pick);
        assert_eq!(app.adjustments[0].brightness, 20);
        assert_eq!(app.adjustments[0].contrast, 15.0);
        assert_eq!(app.adjustments[0].rotation, 90);
        assert_eq!(app.adjustments[2].brightness, -30);
        assert_eq!(app.adjustments[2].contrast, 0.0);
        assert_eq!(app.adjustments[2].rotation, 180);

        // Clear classifications and adjustments again
        app.classifications = vec![Classification::Unflagged; 3];
        app.adjustments = vec![ImageAdjustments::default(); 3];

        // Import from Text
        app.import_classifications(&txt_path).unwrap();
        assert_eq!(app.classifications[0], Classification::Pick);
        assert_eq!(app.classifications[1], Classification::Reject);
        assert_eq!(app.classifications[2], Classification::Pick);
        assert_eq!(app.adjustments[0], ImageAdjustments::default());

        // Clean up
        let _ = std::fs::remove_file(json_path);
        let _ = std::fs::remove_file(txt_path);
    }
}
