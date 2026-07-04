use fast_image_resize as fir;
use image::DynamicImage;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::StatefulProtocol;
use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use crossterm::event::{Event, KeyCode, KeyEventKind};

use crate::commands::{Command, PaletteCommand, get_commands};
use crate::image_worker::{
    FilterType, ImageSource, LoaderRequest, LoaderResponse, ResizeRequest, ScaleMode,
    decode_image_source, process_resize, Brightness, Contrast, PanOffset, CropBox, ImageIntersection,
    SlideshowConfig,
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
            let val = stripped.parse::<T>().map_err(|_| "Invalid positive offset".to_string())?;
            Ok(Self::RelativeAdd(val))
        } else if let Some(stripped) = s.strip_prefix('-') {
            let val = stripped.parse::<T>().map_err(|_| "Invalid negative offset".to_string())?;
            Ok(Self::RelativeSub(val))
        } else {
            let val = s.parse::<T>().map_err(|_| "Invalid absolute value".to_string())?;
            Ok(Self::Absolute(val))
        }
    }
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

    /// Moves the selection to the next image in the queue, wrapping if necessary.
    pub fn next(&mut self) -> bool {
        if self.images.is_empty() {
            return false;
        }
        let old = self.current_index;
        self.current_index = (self.current_index + 1) % self.images.len();
        old != self.current_index
    }

    /// Moves the selection to the previous image in the queue, wrapping if necessary.
    pub fn prev(&mut self) -> bool {
        if self.images.is_empty() {
            return false;
        }
        let old = self.current_index;
        if self.current_index == 0 {
            self.current_index = self.images.len() - 1;
        } else {
            self.current_index -= 1;
        }
        old != self.current_index
    }

    /// Returns true if the image queue contains no images.
    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    /// Returns the filename display name of the currently selected image.
    pub fn get_current_filename(&self) -> &str {
        self.display_names.get(self.current_index).map(|s| s.as_str()).unwrap_or("")
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
}

type PrefetchCache = Arc<Mutex<HashMap<usize, (Arc<DynamicImage>, u32, u32, &'static str)>>>;

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
    protocol_rx: mpsc::Receiver<(StatefulProtocol, (u16, u16))>,
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
    ) -> Result<Self, String> {
        let queue = ImageQueue::new(images, current_index)?;

        let (resize_tx, resize_rx) = mpsc::channel::<ResizeRequest>();
        let (protocol_tx, protocol_rx) = mpsc::channel::<(StatefulProtocol, (u16, u16))>();

        // Spawn background resizing worker thread
        std::thread::spawn(move || {
            let mut resizer = fir::Resizer::new();
            while let Ok(req) = resize_rx.recv() {
                let mut latest_req = req;
                while let Ok(next_req) = resize_rx.try_recv() {
                    latest_req = next_req;
                }
                let rendered_cells = latest_req.rendered_size_cells;
                let protocol = process_resize(latest_req, &mut resizer);
                let _ = protocol_tx.send((protocol, rendered_cells));
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
                    let res = decode_image_source(r.source);
                    let _ = response_tx.send(LoaderResponse {
                        idx: r.idx,
                        result: res,
                        is_prefetch: r.is_prefetch,
                        sequence: r.sequence,
                    });
                }
            }
        });

        let mut app = Self {
            queue,
            filtered_files: Vec::new(),
            filtered_commands: Vec::new(),
            original_image: None,
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
        };

        app.start_load_image();
        Ok(app)
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
                    self.palette_selected_index =
                        self.palette_selected_index.min(self.filtered_files.len() - 1);
                } else {
                    self.palette_selected_index = 0;
                }
            }
            PaletteMode::Command => {
                self.filtered_commands = self.get_filtered_commands_uncached();
                if !self.filtered_commands.is_empty() {
                    self.palette_selected_index =
                        self.palette_selected_index.min(self.filtered_commands.len() - 1);
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
                .map(|(idx, name)| (idx, name.clone()))
                .collect();
        }

        let pattern = nucleo::pattern::Pattern::parse(
            query,
            nucleo::pattern::CaseMatching::Ignore,
            nucleo::pattern::Normalization::Smart,
        );

        #[derive(Clone)]
        struct FileCandidate {
            index: usize,
            name: String,
        }
        impl AsRef<str> for FileCandidate {
            fn as_ref(&self) -> &str {
                &self.name
            }
        }

        let candidates: Vec<FileCandidate> = self
            .queue
            .display_names_lowercase
            .iter()
            .enumerate()
            .map(|(index, name)| FileCandidate {
                index,
                name: name.clone(),
            })
            .collect();

        let mut matches = pattern.match_list(candidates, &mut self.matcher);
        matches.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.index.cmp(&b.0.index)));

        matches
            .into_iter()
            .map(|(candidate, _score)| {
                (candidate.index, self.queue.display_names[candidate.index].clone())
            })
            .collect()
    }

    fn get_filtered_commands_uncached(&mut self) -> Vec<PaletteCommand> {
        let query = &self.palette_query;
        if query.is_empty() {
            let mut list = Vec::new();
            for cmd in get_commands() {
                if cmd.item.show_in_palette {
                    list.push(cmd.clone());
                }
            }
            return list;
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

        let mut candidates = Vec::new();
        for cmd in get_commands() {
            if cmd.item.show_in_palette {
                candidates.push(CmdCandidate { cmd: cmd.clone() });
            }
        }

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
        }
    }

    pub fn open_palette(&mut self, mode: PaletteMode) {
        self.palette_mode = mode;
        self.palette_query.clear();
        self.palette_selected_index = match mode {
            PaletteMode::File => self.queue.current_index,
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
        (|| {
            match prompt_type {
                PromptType::GoToImage => {
                    if self.queue.is_empty() {
                        return;
                    }
                    let input = self.palette_query.trim();
                    let Ok(adj) = input.parse::<Adjustment<usize>>() else {
                        return;
                    };
                    let mut new_idx = self.queue.current_index;
                    match adj {
                        Adjustment::Absolute(val) => {
                            if let Some(val_minus_1) = val.checked_sub(1) {
                                new_idx = val_minus_1.min(self.queue.images.len() - 1);
                            }
                        }
                        Adjustment::RelativeAdd(val) => {
                            new_idx = (self.queue.current_index + val).min(self.queue.images.len() - 1);
                        }
                        Adjustment::RelativeSub(val) => {
                            new_idx = self.queue.current_index.saturating_sub(val);
                        }
                    }
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
            }
        })();
        self.palette_mode = PaletteMode::Closed;
        self.prompt_type = None;
        self.needs_clear_once = true;
    }

    pub fn get_sliding_window_indices(&self) -> Vec<usize> {
        let n = 2; // Cache size N=2 (caches current + 2 before + 2 after)
        let total = self.queue.images.len();
        if total == 0 {
            return Vec::new();
        }
        let mut indices = Vec::new();
        indices.push(self.queue.current_index);
        for i in 1..=n {
            let prev = (self.queue.current_index + total - i) % total;
            let next = (self.queue.current_index + i) % total;
            indices.push(prev);
            indices.push(next);
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

        // Prune any cache entries that are not in the sliding window
        {
            let mut cache = self.prefetch_cache.lock().unwrap();
            cache.retain(|idx, _| window_indices.contains(idx));
        }

        for idx in window_indices {
            if idx == self.queue.current_index {
                continue;
            }

            {
                let cache = self.prefetch_cache.lock().unwrap();
                if cache.contains_key(&idx) {
                    continue;
                }
            }

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

        // Check if the image is in the prefetch cache
        let cached = {
            let mut cache = self.prefetch_cache.lock().unwrap();
            cache.remove(&self.queue.current_index)
        };

        if let Some((img, w, h, icon)) = cached {
            self.current_sequence += 1;
            self.original_image = Some(img);
            self.current_icon = icon;
            self.img_width = w;
            self.img_height = h;
            self.zoom_factor = 1.0;
            self.pan_offset = PanOffset::ZERO;
            self.brightness = Brightness::ZERO;
            self.contrast = Contrast::ZERO;
            self.is_loading = false;
            self.needs_update = true;
            self.zoom_needs_initialization = true;
            self.trigger_prefetch();
            return;
        }

        // Cache miss: load as normal via background loader worker
        self.original_image = None;
        self.image_protocol = None;
        self.is_loading = true;
        self.loading_start_time = Some(Instant::now());
        self.current_sequence += 1;

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
            if resp.sequence < self.current_sequence {
                continue;
            }

            match resp.result {
                Ok((img, w, h, icon)) => {
                    let shared_img = Arc::new(img);
                    if resp.is_prefetch {
                        let window_indices = self.get_sliding_window_indices();
                        if window_indices.contains(&resp.idx) {
                            let mut cache = self.prefetch_cache.lock().unwrap();
                            cache.insert(resp.idx, (shared_img, w, h, icon));
                        }
                    } else if resp.idx == self.queue.current_index {
                        self.img_width = w;
                        self.img_height = h;
                        self.current_icon = icon;
                        self.original_image = Some(shared_img);
                        self.error_message = None;
                        self.zoom_factor = 1.0;
                        self.pan_offset = PanOffset::ZERO;
                        self.brightness = Brightness::ZERO;
                        self.contrast = Contrast::ZERO;
                        self.needs_update = true;
                        self.zoom_needs_initialization = true;
                        self.trigger_prefetch();
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

        if let Ok((protocol, cells)) = self.protocol_rx.try_recv() {
            self.image_protocol = Some(protocol);
            self.rendered_size_cells = cells;
            self.is_loading = false;
            if self.clear_on_protocol_receive {
                self.clear_on_protocol_receive = false;
                self.needs_clear = true;
            }
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

        if let Some(ref img) = self.original_image {
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
                scale,
                crop: CropBox::new(crop_x1, crop_y1, crop_x2, crop_y2),
                intersection: ImageIntersection::new(inter_x1 as u32, inter_y1 as u32, inter_x2 as u32, inter_y2 as u32),
                target_w,
                target_h,
                filter_type: self.filter_type,
                picker: self.picker.clone(),
                brightness: self.brightness,
                contrast: self.contrast,
                rendered_size_cells: rendered_cells,
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
            self.zoom_factor = 1.0;
            self.pan_offset = PanOffset::ZERO;
            self.needs_update = true;
            self.clear_on_protocol_receive = true;
        }
    }

    pub fn current_filename(&self) -> &str {
        if self.queue.is_empty() {
            return "No file loaded";
        }
        self.queue.get_current_filename()
    }

    pub fn next_image(&mut self) {
        if self.queue.next() {
            self.start_load_image();
        }
    }

    pub fn prev_image(&mut self) {
        if self.queue.prev() {
            self.start_load_image();
        }
    }

    /// Handles a Crossterm input event (keyboard or mouse).
    pub fn handle_event(&mut self, ev: Event, terminal_height: u16) {
        if self.palette_mode != PaletteMode::Closed {
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

                            self.palette_selected_index = (self.palette_selected_index
                                + page_size)
                                .min(max_len - 1);
                        }
                    }
                    KeyCode::Char('k')
                        if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
                            && self.palette_selected_index > 0 =>
                    {
                        self.palette_selected_index -= 1;
                    }
                    KeyCode::Char('j')
                        if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) =>
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
