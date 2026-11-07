pub mod adjustments;
pub mod cache;
pub mod classifications;
pub mod events;
pub mod line_editor;
pub mod palette;
pub mod queue;

mod channels;
mod navigation;
mod viewport;

#[cfg(test)]
mod tests;

use crate::config::{InfoBarPosition, SlideshowState};

pub use adjustments::{Adjustment, ImageAdjustments};
pub use cache::PrefetchCache;
pub use classifications::{Classification, ViewMode};
pub use palette::{PaletteMode, PromptType, filter_commands, filter_files};
pub use queue::ImageQueue;

use fast_image_resize as fir;
use image::DynamicImage;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::commands::{Command, PaletteCommand, get_commands};
use crate::imaging::{
    Brightness, CancellationToken, Contrast, DecodedImage, FilterType, ImageSource, LoaderRequest,
    LoaderResponse, PanOffset, RAW_EXIF_PROBE_LIMIT, ResizeRequest, ScaleMode, ZoomFactor,
    decode_image_bytes_cancellable, decode_image_source_cancellable, process_resize,
};

/// Threshold below which navigation is considered fast scrolling (60ms).
/// Background prefetching is suspended while navigating faster than this rate.
pub(crate) const FAST_SCROLL_THRESHOLD: Duration = Duration::from_millis(60);

/// Diagnostic and telemetry instrumentation for image decoding, resizing, and protocol delivery.
#[derive(Debug, Clone, Default)]
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
    /// Image format of the loaded image.
    pub format: Option<image::ImageFormat>,
    /// The original raw/sensor width of the raw image, if any.
    pub raw_width: Option<u32>,
    /// The original raw/sensor height of the raw image, if any.
    pub raw_height: Option<u32>,
}

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

/// Configuration parameters for app components.
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub filter_type: FilterType,
    pub scale_mode: ScaleMode,
    pub disable_thumbnail: bool,
    pub infobar: InfoBarPosition,
}

/// Configuration parameters for directory/archive scanning.
#[derive(Debug, Clone)]
pub struct ScanConfig {
    pub initial_scan_path: std::path::PathBuf,
    pub check_magic: bool,
    pub recursive: bool,
    pub is_cbz: bool,
    pub is_piped: bool,
}

#[cfg(test)]
/// Mock channels used for decoupled synchronous testing of App state transitions.
pub(crate) struct MockChannels {
    /// Receiver for image load requests dispatched by App.
    pub(crate) loader_rx: mpsc::Receiver<LoaderRequest>,
    /// Sender to simulate image decoder responses back to App.
    pub(crate) response_tx: mpsc::Sender<LoaderResponse>,
    /// Receiver for image resize/protocol requests dispatched by App.
    pub(crate) resize_rx: mpsc::Receiver<ResizeRequest>,
    /// Sender to simulate resize/protocol responses back to App.
    pub(crate) protocol_tx: mpsc::Sender<ResizeResponse>,
}

/// The mutually-exclusive display state for the image viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImageDisplayState<'a> {
    /// Actively loading with debounce threshold elapsed and no preview available.
    LoadingSpinner,
    /// Image protocol is ready for display.
    Ready,
    /// An error occurred during loading.
    Error(&'a str),
    /// No image is currently loaded.
    Empty,
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
    pub zoom_factor: ZoomFactor,
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
    /// Triggers a visual clearing of the terminal screen, gated on `should_clear_on_update()` (e.g. Sixel).
    /// Used to clean obsolete graphic cells on discrete updates (like image loads or dismissed dialogues)
    /// without causing constant Kitty/Halfblocks flicker.
    pub needs_clear: bool,
    /// Cell dimensions of the actual rendered Sixel/Kitty image.
    pub rendered_size_cells: (u16, u16),
    /// Percentage display value of the current scaling.
    pub current_zoom_pct: f64,
    /// The active overlay panel state.
    pub palette_mode: PaletteMode,
    /// Input buffer for fuzzy queries or prompt values.
    pub palette_query: line_editor::LineEditor,
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
    prefetch_cache: PrefetchCache,
    /// The slideshow state.
    pub slideshow_state: SlideshowState,
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
    /// Position of the info bar (top, bottom, none).
    pub infobar: InfoBarPosition,
    /// Classifications and adjustments loaded from import files for sources not present in the current queue.
    pub unmapped_classifications: HashMap<String, (Classification, ImageAdjustments)>,
    /// Configuration for directory/archive scanning.
    pub scan_config: ScanConfig,
    /// Active sequence counter for cooperative cancellation in worker threads.
    pub(crate) active_sequence: Arc<AtomicUsize>,
    /// Timestamp of the last navigation action, used to detect fast scrolling.
    pub(crate) last_nav_time: Option<Instant>,
    /// Flag indicating whether background prefetching is currently suspended due to fast scrolling.
    pub(crate) prefetch_suspended: bool,
}

impl App {
    /// Creates a new App controller, launching background threadpools for both
    /// decoding/loading and resizing.
    pub fn new(
        images: Vec<ImageSource>,
        current_index: usize,
        picker: Picker,
        config: AppConfig,
        scan_config: ScanConfig,
    ) -> Result<Self, String> {
        let queue = ImageQueue::new(images, current_index)?;

        let (resize_tx, resize_rx) = mpsc::channel::<ResizeRequest>();
        let (protocol_tx, protocol_rx) = mpsc::channel::<ResizeResponse>();

        let active_sequence = Arc::new(AtomicUsize::new(0));
        let resize_active_seq = Arc::clone(&active_sequence);
        let active_seq = Arc::clone(&active_sequence);

        // Spawn background resizing worker thread
        std::thread::spawn(move || {
            let mut resizer = fir::Resizer::new();
            while let Ok(req) = resize_rx.recv() {
                let mut latest_req = req;
                while let Ok(next_req) = resize_rx.try_recv() {
                    latest_req = next_req;
                }
                if (latest_req.sequence as usize) < resize_active_seq.load(Ordering::Acquire) {
                    continue;
                }
                let rendered_cells = latest_req.rendered_size_cells;
                let target_w = latest_req.target_w;
                let target_h = latest_req.target_h;
                let sequence = latest_req.sequence;
                let (protocol, proc_dur, proto_dur) = process_resize(latest_req, &mut resizer);
                if (sequence as usize) >= resize_active_seq.load(Ordering::Acquire) {
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
            }
        });

        let (loader_tx, loader_rx) = mpsc::channel::<LoaderRequest>();
        let (response_tx, response_rx) = mpsc::channel::<LoaderResponse>();

        // Spawn persistent background loader thread
        let disable_thumbnail = config.disable_thumbnail;
        std::thread::spawn(move || {
            let mut pending_requests = Vec::new();
            let mut thumb_resizer = fir::Resizer::new();
            let thumb_cache_root = crate::imaging::thumbcache::default_thumbnail_cache_root();

            loop {
                // If we don't have any pending requests, block until we receive one
                let mut requests = if pending_requests.is_empty() {
                    match loader_rx.recv() {
                        Ok(req) => vec![req],
                        Err(_) => break, // Channel disconnected, exit thread
                    }
                } else {
                    std::mem::take(&mut pending_requests)
                };

                // Read any other immediately available requests
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

                let mut aborted = false;
                for r in current_requests {
                    let cancel = CancellationToken::with_sequence(r.sequence as usize, &active_seq);
                    if cancel.is_cancelled() {
                        continue;
                    }

                    // Poll channel for newer sequence requests
                    let mut new_reqs = Vec::new();
                    while let Ok(nr) = loader_rx.try_recv() {
                        new_reqs.push(nr);
                    }
                    if !new_reqs.is_empty() {
                        let new_max_seq = new_reqs.iter().map(|nr| nr.sequence).max().unwrap_or(0);
                        if new_max_seq > highest_seq {
                            // Abort current sequence processing
                            pending_requests = new_reqs;
                            aborted = true;
                            break;
                        }
                    }

                    if cancel.is_cancelled() {
                        continue;
                    }

                    let start = std::time::Instant::now();

                    // Try to load limited bytes first and extract thumbnail if this is an active cold load
                    let mut bytes_opt = None;
                    let is_raw = r.source.is_raw();
                    let limit = if is_raw {
                        RAW_EXIF_PROBE_LIMIT
                    } else {
                        256 * 1024
                    };

                    let local_path = match &r.source {
                        ImageSource::Local(p) => Some(p.clone()),
                        _ => None,
                    };

                    let mut has_thumbnail = false;

                    if !disable_thumbnail {
                        if cancel.is_cancelled() {
                            continue;
                        }
                        let read_res = crate::imaging::read_source_bytes_limited(&r.source, limit);
                        if let Ok(partial_bytes) = read_res {
                            if cancel.is_cancelled() {
                                continue;
                            }
                            if let Some((thumb_img, real_w, real_h)) =
                                crate::imaging::decode_thumbnail_and_dimensions(&partial_bytes)
                                && !cancel.is_cancelled()
                            {
                                // Send thumbnail placeholder immediately
                                let thumb_dur = start.elapsed();
                                let _ = response_tx.send(LoaderResponse {
                                    idx: r.idx,
                                    result: Ok(DecodedImage {
                                        image: thumb_img,
                                        width: real_w,
                                        height: real_h,
                                        format: Some(image::ImageFormat::Jpeg),
                                        disk_size: 0, // Filled in by final high-res response
                                        raw_width: Some(real_w),
                                        raw_height: Some(real_h),
                                    }),
                                    is_prefetch: r.is_prefetch,
                                    sequence: r.sequence,
                                    decode_duration: thumb_dur,
                                    is_thumbnail: true,
                                });
                                has_thumbnail = true;
                            }
                            if partial_bytes.len() < limit {
                                // If the file was smaller than the limit, we have already loaded it fully
                                bytes_opt = Some(partial_bytes);
                            }
                        }

                        // If EXIF thumbnail was not found, check Freedesktop thumbnail cache
                        if !has_thumbnail
                            && !cancel.is_cancelled()
                            && let Some(ref path) = local_path
                            && let Some(ref cache_root) = thumb_cache_root
                            && let Some(cached_thumb) =
                                crate::imaging::thumbcache::load_cached_thumbnail(cache_root, path)
                        {
                            let (mut real_w, mut real_h) = image::image_dimensions(path)
                                .unwrap_or((cached_thumb.width(), cached_thumb.height()));
                            if cached_thumb.width() != cached_thumb.height()
                                && (real_w > real_h)
                                    != (cached_thumb.width() > cached_thumb.height())
                            {
                                std::mem::swap(&mut real_w, &mut real_h);
                            }
                            let thumb_dur = start.elapsed();
                            let _ = response_tx.send(LoaderResponse {
                                idx: r.idx,
                                result: Ok(DecodedImage {
                                    image: cached_thumb,
                                    width: real_w,
                                    height: real_h,
                                    format: Some(image::ImageFormat::Png),
                                    disk_size: 0,
                                    raw_width: Some(real_w),
                                    raw_height: Some(real_h),
                                }),
                                is_prefetch: r.is_prefetch,
                                sequence: r.sequence,
                                decode_duration: thumb_dur,
                                is_thumbnail: true,
                            });
                            has_thumbnail = true;
                        }
                    }

                    if cancel.is_cancelled() {
                        continue;
                    }

                    // Decode the full resolution image
                    let final_bytes = if let Some(bytes) = bytes_opt {
                        Some(bytes)
                    } else {
                        crate::imaging::read_source_bytes(&r.source).ok()
                    };

                    if cancel.is_cancelled() {
                        continue;
                    }

                    let res = if let Some(ref bytes) = final_bytes {
                        decode_image_bytes_cancellable(bytes, &r.source, &cancel)
                    } else {
                        decode_image_source_cancellable(r.source, &cancel)
                    };

                    if !cancel.is_cancelled() {
                        if let Ok(ref decoded) = res
                            && !has_thumbnail
                            && !disable_thumbnail
                            && let Some(ref path) = local_path
                            && let Some(ref cache_root) = thumb_cache_root
                        {
                            crate::imaging::thumbcache::save_cached_thumbnail(
                                cache_root,
                                &mut thumb_resizer,
                                path,
                                &decoded.image,
                            );
                        }

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

                if aborted {
                    continue;
                }
            }
        });

        let mut app = Self::with_channels(
            queue,
            picker,
            config,
            scan_config,
            loader_tx,
            response_rx,
            resize_tx,
            protocol_rx,
            active_sequence,
        );

        app.start_load_image();
        Ok(app)
    }

    /// Constructs an `App` instance initialized with the given channel pairs.
    #[allow(clippy::too_many_arguments)]
    fn with_channels(
        queue: ImageQueue,
        picker: Picker,
        config: AppConfig,
        scan_config: ScanConfig,
        loader_tx: mpsc::Sender<LoaderRequest>,
        response_rx: mpsc::Receiver<LoaderResponse>,
        resize_tx: mpsc::Sender<ResizeRequest>,
        protocol_rx: mpsc::Receiver<ResizeResponse>,
        active_sequence: Arc<AtomicUsize>,
    ) -> Self {
        Self {
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
            zoom_factor: ZoomFactor::DEFAULT,
            pan_offset: PanOffset::ZERO,
            running: true,
            error_message: None,
            last_widget_size: (0, 0),
            needs_update: true,
            needs_clear: true,
            rendered_size_cells: (0, 0),
            current_zoom_pct: 100.0,
            palette_mode: PaletteMode::Closed,
            palette_query: line_editor::LineEditor::new(),
            palette_selected_index: 0,
            palette_width: 0,
            palette_height: 0,
            prompt_type: None,
            filter_type: config.filter_type,
            scale_mode: config.scale_mode,
            matcher: nucleo::Matcher::new(nucleo::Config::DEFAULT),
            resize_tx,
            protocol_rx,
            loader_tx,
            response_rx,
            current_sequence: 0,
            is_loading: false,
            loading_start_time: None,
            clear_on_protocol_receive: false,
            zoom_needs_initialization: false,
            prefetch_cache: Arc::new(Mutex::new(HashMap::new())),
            slideshow_state: SlideshowState::OFF,
            slideshow_last_transition: std::time::Instant::now(),
            stats: StatsForNerds::default(),
            last_info_toggle: None,
            disable_thumbnail: config.disable_thumbnail,
            view_mode: ViewMode::Default,
            infobar: config.infobar,
            unmapped_classifications: HashMap::new(),
            scan_config,
            active_sequence,
            last_nav_time: None,
            prefetch_suspended: false,
        }
    }

    #[cfg(test)]
    /// Initializes an App instance with decoupled, synchronous mock channels.
    /// Does NOT spawn any background OS worker threads.
    pub(crate) fn new_mocked(
        images: Vec<ImageSource>,
        current_index: usize,
        picker: Picker,
        config: AppConfig,
        scan_config: ScanConfig,
    ) -> Result<(Self, MockChannels), String> {
        let queue = ImageQueue::new(images, current_index)?;
        let (resize_tx, mock_resize_rx) = mpsc::channel::<ResizeRequest>();
        let (mock_protocol_tx, protocol_rx) = mpsc::channel::<ResizeResponse>();
        let (loader_tx, mock_loader_rx) = mpsc::channel::<LoaderRequest>();
        let (mock_response_tx, response_rx) = mpsc::channel::<LoaderResponse>();

        let active_sequence = Arc::new(AtomicUsize::new(0));
        let mut app = Self::with_channels(
            queue,
            picker,
            config,
            scan_config,
            loader_tx,
            response_rx,
            resize_tx,
            protocol_rx,
            active_sequence,
        );
        app.start_load_image();
        let channels = MockChannels {
            loader_rx: mock_loader_rx,
            response_tx: mock_response_tx,
            resize_rx: mock_resize_rx,
            protocol_tx: mock_protocol_tx,
        };
        Ok((app, channels))
    }

    /// Returns the active viewport display state.
    pub(crate) fn display_state(&self) -> ImageDisplayState<'_> {
        let show_loading = self.is_loading
            && self.image_protocol.is_none()
            && self.thumbnail_image.is_none()
            && self
                .loading_start_time
                .is_some_and(|t| t.elapsed() > Duration::from_millis(150));

        if show_loading {
            ImageDisplayState::LoadingSpinner
        } else if self.image_protocol.is_some() {
            ImageDisplayState::Ready
        } else if let Some(ref err) = self.error_message {
            ImageDisplayState::Error(err.as_str())
        } else {
            ImageDisplayState::Empty
        }
    }

    /// Sets an error state, terminating loading and clearing image protocol and buffer data.
    pub(crate) fn set_error_state(&mut self, msg: impl Into<String>) {
        self.error_message = Some(msg.into());
        self.is_loading = false;
        self.loading_start_time = None;
        self.original_image = None;
        self.thumbnail_image = None;
        self.image_protocol = None;
    }

    /// Returns cached filtered files matching the current search query.
    pub fn get_filtered_files(&self) -> &[(usize, String)] {
        &self.filtered_files
    }

    /// Returns cached filtered commands matching the current search query.
    pub fn get_filtered_commands(&self) -> &[PaletteCommand] {
        &self.filtered_commands
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
        let queue = &self.queue;
        let view_mode = self.view_mode;
        filter_files(
            self.palette_query.value(),
            queue.relative_paths(),
            queue.relative_paths_lowercase(),
            &mut self.matcher,
            |idx| queue.is_visible(idx, view_mode),
        )
    }

    fn get_filtered_commands_uncached(&mut self) -> Vec<PaletteCommand> {
        filter_commands(self.palette_query.value(), &mut self.matcher)
    }

    /// Returns the human-readable string name of the active filter type.
    pub fn filter_name(&self) -> &'static str {
        match self.filter_type {
            FilterType::Nearest => "Nearest",
            FilterType::Linear => "Linear",
            FilterType::Cubic => "Cubic",
            FilterType::Mitchell => "Mitchell",
            FilterType::Gaussian => "Gaussian",
            FilterType::Lanczos => "Lanczos",
            FilterType::Hamming => "Hamming",
        }
    }

    /// Cycles through available image scaling filters.
    pub fn cycle_filter(&mut self) {
        if self.is_loading {
            return;
        }
        self.filter_type = match self.filter_type {
            FilterType::Nearest => FilterType::Hamming,
            FilterType::Hamming => FilterType::Linear,
            FilterType::Linear => FilterType::Cubic,
            FilterType::Cubic => FilterType::Mitchell,
            FilterType::Mitchell => FilterType::Gaussian,
            FilterType::Gaussian => FilterType::Lanczos,
            FilterType::Lanczos => FilterType::Nearest,
        };
        self.needs_update = true;
    }

    /// Executes a discrete user command, updating internal application state.
    pub fn execute_command(&mut self, cmd: Command) {
        match cmd {
            Command::ResetView => self.reset_view(),
            Command::ResetImage => self.reset_image(),
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
                self.filter_type = FilterType::Linear;
                self.needs_update = true;
            }
            Command::SetFilterCubic => {
                self.filter_type = FilterType::Cubic;
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
                self.filter_type = FilterType::Lanczos;
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
                let current_sec = self.slideshow_state.seconds();
                let new_delay =
                    std::time::Duration::from_secs(current_sec.saturating_add(1).max(1) as u64);
                self.slideshow_state = match self.slideshow_state {
                    SlideshowState::Playing { .. } => SlideshowState::Playing { delay: new_delay },
                    SlideshowState::Paused { .. } => SlideshowState::Paused { delay: new_delay },
                    SlideshowState::Stopped => SlideshowState::Playing { delay: new_delay },
                };
                self.slideshow_last_transition = std::time::Instant::now();
            }
            Command::SlideshowDecrease => {
                let current_sec = self.slideshow_state.seconds();
                let next_sec = current_sec.saturating_sub(1);
                self.slideshow_state = if next_sec == 0 {
                    SlideshowState::Stopped
                } else {
                    let new_delay = std::time::Duration::from_secs(next_sec as u64);
                    match self.slideshow_state {
                        SlideshowState::Playing { .. } => {
                            SlideshowState::Playing { delay: new_delay }
                        }
                        SlideshowState::Paused { .. } => {
                            SlideshowState::Paused { delay: new_delay }
                        }
                        SlideshowState::Stopped => SlideshowState::Stopped,
                    }
                };
                self.slideshow_last_transition = std::time::Instant::now();
            }
            Command::SetSlideshow => self.open_prompt(PromptType::SetSlideshow),
            Command::ToggleThumbnail => {
                if self.thumbnail_image.is_some() {
                    self.show_thumbnail_only = !self.show_thumbnail_only;
                    self.needs_update = true;
                    self.needs_clear = true;
                }
            }
            Command::ShowInfo => {
                if self.palette_mode == PaletteMode::Info {
                    if self
                        .last_info_toggle
                        .is_none_or(|t| t.elapsed() > std::time::Duration::from_millis(200))
                    {
                        self.palette_mode = PaletteMode::Closed;
                        self.needs_clear = true;
                        self.last_info_toggle = Some(std::time::Instant::now());
                    }
                } else {
                    self.palette_mode = PaletteMode::Info;
                    self.palette_height = 19;
                    self.needs_clear = true;
                    self.last_info_toggle = Some(std::time::Instant::now());
                }
            }
            Command::MarkPick => self.mark_pick(),
            Command::MarkReject => self.mark_reject(),
            Command::Unflag => self.unflag_image(),
            Command::RefreshFiles => {
                if let Err(e) = self.refresh_files() {
                    self.error_message = Some(e);
                }
            }
            Command::CycleView => self.cycle_view_mode(),
            Command::SetViewDefault => self.set_view_mode(ViewMode::Default),
            Command::SetViewUnflagged => self.set_view_mode(ViewMode::Unflagged),
            Command::SetViewPicks => self.set_view_mode(ViewMode::Picks),
            Command::SetViewRejects => self.set_view_mode(ViewMode::Rejects),
            Command::SetViewAll => self.set_view_mode(ViewMode::All),
            Command::SetInfoBarTop => {
                self.infobar = InfoBarPosition::Top;
                self.needs_update = true;
                self.needs_clear = true;
            }
            Command::SetInfoBarBottom => {
                self.infobar = InfoBarPosition::Bottom;
                self.needs_update = true;
                self.needs_clear = true;
            }
            Command::SetInfoBarNone => {
                self.infobar = InfoBarPosition::None;
                self.needs_update = true;
                self.needs_clear = true;
            }
            Command::CycleInfoBar => {
                self.infobar = match self.infobar {
                    InfoBarPosition::Bottom => InfoBarPosition::Top,
                    InfoBarPosition::Top => InfoBarPosition::None,
                    InfoBarPosition::None => InfoBarPosition::Bottom,
                };
                self.needs_update = true;
                self.needs_clear = true;
            }
            Command::ToggleSlideshowPause => {
                self.slideshow_state = match self.slideshow_state {
                    SlideshowState::Playing { delay } => SlideshowState::Paused { delay },
                    SlideshowState::Paused { delay } => SlideshowState::Playing { delay },
                    SlideshowState::Stopped => SlideshowState::Stopped,
                };
                self.needs_clear = true;
            }
        }
    }

    /// Opens an overlay palette (Command, File, etc.) and clears previous query state.
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
                .relative_paths()
                .iter()
                .map(|name| name.chars().count())
                .max()
                .unwrap_or(0) as u16,
            PaletteMode::Command => get_commands()
                .iter()
                .filter(|cmd| cmd.item.show_in_palette)
                .map(|cmd| cmd.item.name.chars().count() + 3 + cmd.item.description.chars().count())
                .max()
                .unwrap_or(0) as u16,
            _ => 0,
        };

        self.palette_width = max_text_width + 5;
        self.palette_height = 0;
    }

    /// Opens a single-line input prompt for value entry.
    pub fn open_prompt(&mut self, prompt_type: PromptType) {
        self.palette_mode = PaletteMode::Prompt;
        self.prompt_type = Some(prompt_type);
        self.palette_query.clear();
        self.palette_selected_index = 0;
        self.palette_width = 45;
        self.palette_height = 0;
        self.needs_clear = true;
    }

    /// Executes an entered value from the interactive input prompt.
    pub fn execute_prompt(&mut self, prompt_type: PromptType) {
        (|| match prompt_type {
            PromptType::GoToImage => {
                if self.queue.is_empty() {
                    return;
                }
                let visible: Vec<usize> = (0..self.queue.len())
                    .filter(|&idx| self.is_visible(idx))
                    .collect();
                if visible.is_empty() {
                    return;
                }
                let input = self.palette_query.value().trim();
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
                let idx = self.queue.current_index;
                if let Some(adj_slot) = self.queue.adjustments_mut().get_mut(idx) {
                    let input = self.palette_query.value().trim();
                    let Ok(adj) = input.parse::<Adjustment<i32>>() else {
                        return;
                    };
                    let old = adj_slot.brightness;
                    match adj {
                        Adjustment::Absolute(val) => adj_slot.brightness = Brightness::new(val),
                        Adjustment::RelativeAdd(val) => adj_slot.brightness.adjust(val),
                        Adjustment::RelativeSub(val) => {
                            adj_slot.brightness.adjust(val.saturating_neg())
                        }
                    }
                    if old != adj_slot.brightness {
                        self.needs_update = true;
                    }
                }
            }
            PromptType::SetContrast => {
                if self.original_image.is_none() {
                    return;
                }
                let idx = self.queue.current_index;
                if let Some(adj_slot) = self.queue.adjustments_mut().get_mut(idx) {
                    let input = self.palette_query.value().trim();
                    let Ok(adj) = input.parse::<Adjustment<f32>>() else {
                        return;
                    };
                    let mut next = adj_slot.contrast;
                    match adj {
                        Adjustment::Absolute(val) => next = Contrast::new(val),
                        Adjustment::RelativeAdd(val) => next.adjust(val),
                        Adjustment::RelativeSub(val) => next.adjust(-val),
                    }
                    if adj_slot.contrast.update(next.value()) {
                        self.needs_update = true;
                    }
                }
            }
            PromptType::SetSlideshow => {
                let input = self.palette_query.value().trim();
                let Ok(adj) = input.parse::<Adjustment<u32>>() else {
                    return;
                };
                let mut new_val = self.slideshow_state.seconds();
                match adj {
                    Adjustment::Absolute(val) => new_val = val,
                    Adjustment::RelativeAdd(val) => new_val = new_val.saturating_add(val),
                    Adjustment::RelativeSub(val) => new_val = new_val.saturating_sub(val),
                }
                if new_val != self.slideshow_state.seconds() {
                    self.slideshow_state = SlideshowState::new(new_val);
                    self.slideshow_last_transition = std::time::Instant::now();
                }
            }
        })();
        self.palette_mode = PaletteMode::Closed;
        self.prompt_type = None;
        self.needs_clear = true;
    }
}

#[cfg(test)]
/// Helper to instantiate a test App with mock channels without spawning worker threads.
pub(crate) fn setup_test_app_with_channels() -> (App, MockChannels) {
    let images = vec![
        ImageSource::Local(std::path::PathBuf::from("img1.png")),
        ImageSource::Local(std::path::PathBuf::from("img2.png")),
    ];
    let picker = Picker::halfblocks();
    let app_config = AppConfig {
        filter_type: crate::imaging::FilterType::Nearest,
        scale_mode: crate::imaging::ScaleMode::Shrink,
        disable_thumbnail: false,
        infobar: InfoBarPosition::Bottom,
    };
    let scan_config = ScanConfig {
        initial_scan_path: std::path::PathBuf::from("."),
        check_magic: false,
        recursive: false,
        is_cbz: false,
        is_piped: false,
    };
    App::new_mocked(images, 0, picker, app_config, scan_config).unwrap()
}

#[cfg(test)]
/// Helper to instantiate a test App without spawning worker threads.
pub(crate) fn setup_test_app() -> App {
    let (app, _) = setup_test_app_with_channels();
    app
}
