pub mod adjustments;
pub mod cache;
pub mod classifications;
pub mod events;
pub use crate::commands::LineEditor;
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
pub use palette::{PaletteMode, PromptType};
pub use queue::ImageQueue;

use image::DynamicImage;
use ratatui_image::picker::Picker;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::commands::{Command, get_commands};
use crate::imaging::{
    Brightness, Contrast, FilterType, ImageSource, LoaderRequest, LoaderResponse, PanOffset,
    ResizeRequest, ScaleMode, ViewportProtocol, ZoomFactor,
};

/// Threshold below which navigation is considered fast scrolling (60ms).
/// Background prefetching is suspended while navigating faster than this rate.
pub const FAST_SCROLL_THRESHOLD: Duration = Duration::from_millis(60);

/// Tracks the background loader lifecycle for the currently selected image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoadPhase {
    /// Full-resolution image is resident (or queue is empty / errored); no foreground loader work pending.
    #[default]
    Complete,
    /// Fast-scroll `ThumbnailOnly` request is in flight on the loader worker; full decode is deferred.
    ThumbnailInFlightDeferred,
    /// Thumbnail is resident in memory; waiting for fast-scroll settle (`tick()`) to dispatch `FullOnly`.
    FullDeferred,
    /// Full-resolution decode (`ThumbnailAndFull` or `FullOnly`) is in flight on the loader worker.
    FullInFlight,
}

impl LoadPhase {
    /// Returns true if the active image still has pending or in-flight work on the loader pipeline.
    #[inline]
    pub fn is_pending(self) -> bool {
        !matches!(self, Self::Complete)
    }

    /// Returns true if full-resolution decoding was deferred during fast scrolling and awaits promotion.
    #[inline]
    pub fn needs_promotion(self) -> bool {
        matches!(self, Self::ThumbnailInFlightDeferred | Self::FullDeferred)
    }
}

/// Tracks the lifecycle of viewport protocol resizing and initial zoom calculation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResizeState {
    /// Viewport protocol is up-to-date for `last_widget_size` (or no image is loaded).
    #[default]
    Idle,
    /// Viewport state changed; next `update_layout` must dispatch a `ResizeRequest`.
    Dirty {
        /// Whether `update_protocol` must initialize `zoom_factor` and `pan_offset` from `scale_mode`.
        init_zoom: bool,
    },
    /// A `ResizeRequest` is currently in flight on the resizer worker thread.
    InFlight,
}

impl ResizeState {
    /// Creates a new `ResizeState` in the `Dirty` state with initial zoom pending disabled.
    pub const fn new_dirty() -> Self {
        Self::Dirty { init_zoom: false }
    }

    /// Marks the resize state as dirty, requesting a new `ResizeRequest` on next layout update.
    /// Preserves `init_zoom: true` if an initial zoom calculation was already pending.
    pub fn mark_dirty(&mut self) {
        let preserve_init = matches!(self, Self::Dirty { init_zoom: true });
        *self = Self::Dirty {
            init_zoom: preserve_init,
        };
    }

    /// Marks the resize state as dirty and flags initial zoom calculation as pending.
    pub fn mark_dirty_with_init_zoom(&mut self) {
        *self = Self::Dirty { init_zoom: true };
    }

    /// Resets the resize state to idle.
    pub fn reset_idle(&mut self) {
        *self = Self::Idle;
    }

    /// Sets the status to `InFlight` if `sent` is true, or `Idle` otherwise.
    pub fn set_in_flight(&mut self, sent: bool) {
        *self = if sent { Self::InFlight } else { Self::Idle };
    }

    /// Completes an in-flight resize operation, transitioning back to `Idle` (if still in flight).
    pub fn complete_in_flight(&mut self) {
        if matches!(self, Self::InFlight) {
            *self = Self::Idle;
        }
    }

    /// Returns `true` if a new resize request is dirty/pending.
    pub fn is_dirty(&self) -> bool {
        matches!(self, Self::Dirty { .. })
    }

    /// Returns `true` if a resize request is currently in flight on the resizer worker.
    pub fn is_in_flight(&self) -> bool {
        matches!(self, Self::InFlight)
    }

    /// Returns `true` if the resizer pipeline is busy (either dirty or in flight).
    pub fn is_busy(&self) -> bool {
        !matches!(self, Self::Idle)
    }

    /// Takes the `init_zoom` flag, returning its value and resetting it to `false`.
    pub fn take_init_zoom(&mut self) -> bool {
        match self {
            Self::Dirty { init_zoom } => std::mem::replace(init_zoom, false),
            _ => false,
        }
    }
}

/// Tracks terminal screen clearing requests across immediate UI transitions and deferred protocol updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScreenClear {
    /// No terminal clear is scheduled.
    #[default]
    None,
    /// Defer clearing the terminal until the next `ResizeResponse` protocol arrives from the worker thread.
    OnProtocolReceive,
    /// Clear the terminal immediately before the next frame draw.
    Pending,
}

impl ScreenClear {
    /// Schedules an immediate terminal clear before the next frame draw.
    pub fn request_now(&mut self) {
        *self = Self::Pending;
    }

    /// Defers screen clearing until a new image protocol is received, unless a clear is already pending.
    pub fn defer_until_protocol(&mut self) {
        if *self == Self::None {
            *self = Self::OnProtocolReceive;
        }
    }

    /// Handles arrival of a new protocol, promoting `OnProtocolReceive` (or active palette) to `Pending`.
    pub fn on_protocol_received(&mut self, palette_open: bool) {
        if *self == Self::OnProtocolReceive || palette_open {
            *self = Self::Pending;
        }
    }

    /// Consumes the pending clear flag if scheduled, returning `true`.
    pub fn take_pending(&mut self) -> bool {
        if *self == Self::Pending {
            *self = Self::None;
            true
        } else {
            false
        }
    }

    /// Returns `true` if an immediate terminal clear is pending.
    pub fn is_pending(&self) -> bool {
        matches!(self, Self::Pending)
    }
}

/// Tracks whether the application is awaiting the initial displayable frame for the current image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InitialLoadState {
    /// An image (or thumbnail placeholder) is already displayed, or no load is active.
    #[default]
    Idle,
    /// Waiting for the initial decode/protocol of a load started at `started_at`.
    Loading {
        /// Timestamp when the load began, used to debounce the 150ms loading spinner.
        started_at: Instant,
        /// Whether the one-shot Frame-1 barrier wait is still pending for this load.
        frame_ready_pending: bool,
    },
}

impl InitialLoadState {
    /// Begins a new load starting at the current instant.
    pub fn start() -> Self {
        Self::Loading {
            started_at: Instant::now(),
            frame_ready_pending: true,
        }
    }

    /// Begins a new load with an explicit starting timestamp.
    #[cfg(test)]
    pub fn start_at(started_at: Instant) -> Self {
        Self::Loading {
            started_at,
            frame_ready_pending: true,
        }
    }

    /// Returns `true` if image loading is currently active.
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Loading { .. })
    }

    /// Returns `true` if the one-shot Frame-1 barrier wait is still pending.
    #[cfg(test)]
    pub fn is_frame_ready_pending(&self) -> bool {
        matches!(
            self,
            Self::Loading {
                frame_ready_pending: true,
                ..
            }
        )
    }

    /// Consumes the one-shot Frame-1 barrier flag, returning `true` if it was pending.
    pub fn take_frame_ready_pending(&mut self) -> bool {
        if let Self::Loading {
            ref mut frame_ready_pending,
            ..
        } = *self
        {
            std::mem::replace(frame_ready_pending, false)
        } else {
            false
        }
    }

    /// Returns `true` if loading duration has exceeded `debounce` threshold.
    pub fn should_show_spinner(&self, debounce: Duration) -> bool {
        matches!(
            self,
            Self::Loading { started_at, .. } if started_at.elapsed() > debounce
        )
    }
}

/// Tracks user navigation cadence to detect fast scrolling and suspend/resume background work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScrollCadence {
    /// No navigation has occurred yet.
    #[default]
    Idle,
    /// Normal navigation cadence (or fast-scroll settle period has elapsed); prefetching is active.
    Settled {
        /// Timestamp of the most recent navigation event.
        last_nav: Instant,
    },
    /// Rapid navigation detected within `FAST_SCROLL_THRESHOLD`; full decode and neighbor prefetch are suspended.
    FastScrolling {
        /// Timestamp of the most recent navigation event.
        last_nav: Instant,
    },
}

impl ScrollCadence {
    /// Returns the timestamp of the last navigation action, if any.
    pub fn last_nav_time(&self) -> Option<Instant> {
        match *self {
            Self::Idle => None,
            Self::Settled { last_nav } | Self::FastScrolling { last_nav } => Some(last_nav),
        }
    }

    /// Records a new navigation event, transitioning to `FastScrolling` if within `FAST_SCROLL_THRESHOLD`.
    /// Returns `true` if fast scrolling was detected.
    pub fn record_navigation(&mut self, now: Instant) -> bool {
        let is_fast = self
            .last_nav_time()
            .is_some_and(|prev| now.duration_since(prev) < FAST_SCROLL_THRESHOLD);
        *self = if is_fast {
            Self::FastScrolling { last_nav: now }
        } else {
            Self::Settled { last_nav: now }
        };
        is_fast
    }

    /// Returns `true` if neighbor prefetching is currently suspended due to rapid navigation.
    pub fn is_prefetch_suspended(&self) -> bool {
        matches!(self, Self::FastScrolling { .. })
    }

    /// Returns `true` if navigation has settled (or no navigation has occurred).
    pub fn is_settled(&self) -> bool {
        self.last_nav_time()
            .is_none_or(|t| t.elapsed() >= FAST_SCROLL_THRESHOLD)
    }

    /// Transitions from `FastScrolling` to `Settled` if the elapsed time exceeds `FAST_SCROLL_THRESHOLD`.
    /// Returns `true` if a transition from `FastScrolling` to `Settled` occurred.
    pub fn poll_settled(&mut self) -> bool {
        if let Self::FastScrolling { last_nav } = *self
            && last_nav.elapsed() >= FAST_SCROLL_THRESHOLD
        {
            *self = Self::Settled { last_nav };
            return true;
        }
        false
    }
}

pub use crate::imaging::ResizeResponse;

/// Configuration parameters for app components.
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub filter_type: FilterType,
    pub scale_mode: ScaleMode,
    pub disable_thumbnail: bool,
    pub infobar: InfoBarPosition,
    /// Active graphics protocol used by the app.
    pub active_protocol: crate::config::ActiveProtocol,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            filter_type: FilterType::Nearest,
            scale_mode: ScaleMode::Shrink,
            disable_thumbnail: false,
            infobar: InfoBarPosition::Bottom,
            active_protocol: crate::config::ActiveProtocol::Halfblocks,
        }
    }
}

/// Configuration parameters for directory, archive, or piped input scanning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanConfig {
    /// Input was piped via stdin; rescanning (`refresh_files`) is not supported.
    Piped,
    /// Input is a single CBZ/ZIP archive at the given path.
    Cbz(std::path::PathBuf),
    /// Input is a filesystem directory or file path scanned with magic-byte and recursion options.
    Directory {
        /// Initial file or directory path to scan.
        path: std::path::PathBuf,
        /// Whether to verify file magic headers in addition to extensions.
        check_magic: bool,
        /// Whether to traverse subdirectories recursively.
        recursive: bool,
    },
}

#[cfg(test)]
/// Mock channels used for decoupled synchronous testing of App state transitions.
pub struct MockChannels {
    /// Receiver for image load requests dispatched by App.
    pub loader_rx: mpsc::Receiver<LoaderRequest>,
    /// Sender to simulate image decoder responses back to App.
    pub response_tx: mpsc::Sender<LoaderResponse>,
    /// Receiver for image resize/protocol requests dispatched by App.
    pub resize_rx: mpsc::Receiver<ResizeRequest>,
    /// Sender to simulate resize/protocol responses back to App.
    pub protocol_tx: mpsc::Sender<ResizeResponse>,
}

pub use crate::ui::{ImageDisplayState, StatsForNerds, ThumbnailTelemetry};

/// The central application state controller.
pub struct App {
    /// The encapsulated image queue and navigation state.
    pub queue: ImageQueue,
    /// Cached filtered file indices matching current search query.
    pub filtered_files: Vec<usize>,
    /// Cached filtered command indices matching current search query.
    pub filtered_commands: Vec<usize>,
    palette_scratch: Vec<(u32, usize)>,
    /// Shared reference to the decoded dynamic image.
    pub original_image: Option<Arc<DynamicImage>>,
    /// Extracted thumbnail placeholder image, if any.
    pub thumbnail_image: Option<Arc<DynamicImage>>,
    /// If true, display the low-res thumbnail image placeholder only.
    pub show_thumbnail_only: bool,
    /// Active render state protocol (Kitty, Sixel, Halfblocks).
    pub image_protocol: Option<ViewportProtocol>,
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
    /// Tracks the background resize worker pipeline and initial zoom calculation.
    pub resize_state: ResizeState,
    /// Lifecycle state of background loading for the currently selected image.
    pub load_phase: LoadPhase,
    /// Terminal screen clear coordination across immediate and deferred updates.
    pub screen_clear: ScreenClear,
    /// Cell dimensions of the actual rendered Sixel/Kitty image.
    pub rendered_size_cells: (u16, u16),
    /// Percentage display value of the current scaling.
    pub current_zoom_pct: f64,
    /// The active overlay panel state.
    pub palette_mode: PaletteMode,
    /// Input buffer for fuzzy queries or prompt values.
    pub palette_query: LineEditor,
    /// Selected row inside the fuzzy matcher list.
    pub palette_selected_index: usize,
    /// Freezed layout width of the popup dialogue.
    pub palette_width: u16,
    /// Dynamic height of the popup dialogue.
    pub palette_height: u16,
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
    /// Lifecycle of the active foreground image decode/protocol load.
    pub initial_load: InitialLoadState,
    /// Prefetch cache storing decoded images in the visible sliding window.
    pub prefetch_cache: PrefetchCache,
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
    pub active_sequence: Arc<AtomicUsize>,
    /// User navigation cadence used to detect fast scrolling and throttle prefetching.
    pub scroll_cadence: ScrollCadence,
    /// Active runtime terminal graphics protocol and its protocol-specific options.
    pub active_protocol: crate::config::ActiveProtocol,
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

        crate::imaging::spawn_resize_worker(resize_rx, protocol_tx, resize_active_seq);

        let (loader_tx, loader_rx) = mpsc::channel::<LoaderRequest>();
        let (response_tx, response_rx) = mpsc::channel::<LoaderResponse>();

        crate::imaging::spawn_loader_pool(
            loader_rx,
            response_tx,
            active_seq,
            config.disable_thumbnail,
        );

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
            palette_scratch: Vec::new(),
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
            resize_state: ResizeState::new_dirty(),
            load_phase: LoadPhase::Complete,
            screen_clear: ScreenClear::Pending,
            rendered_size_cells: (0, 0),
            current_zoom_pct: 100.0,
            palette_mode: PaletteMode::Closed,
            palette_query: LineEditor::new(),
            palette_selected_index: 0,
            palette_width: 0,
            palette_height: 0,
            filter_type: config.filter_type,
            scale_mode: config.scale_mode,
            matcher: nucleo::Matcher::new(nucleo::Config::DEFAULT),
            resize_tx,
            protocol_rx,
            loader_tx,
            response_rx,
            current_sequence: 0,
            initial_load: InitialLoadState::Idle,
            prefetch_cache: PrefetchCache::new(),
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
            scroll_cadence: ScrollCadence::Idle,
            active_protocol: config.active_protocol,
        }
    }

    #[cfg(test)]
    /// Initializes an App instance with decoupled, synchronous mock channels.
    /// Does NOT spawn any background OS worker threads.
    pub fn new_mocked(
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
    pub fn display_state(&self) -> ImageDisplayState<'_> {
        Self::compute_display_state(
            self.image_protocol.is_some(),
            self.thumbnail_image.is_some(),
            self.initial_load
                .should_show_spinner(Duration::from_millis(150)),
            self.error_message.as_deref(),
        )
    }

    #[inline]
    fn compute_display_state<'a>(
        has_protocol: bool,
        has_thumbnail: bool,
        show_spinner: bool,
        error_message: Option<&'a str>,
    ) -> ImageDisplayState<'a> {
        let show_loading = !has_protocol && !has_thumbnail && show_spinner;
        if show_loading {
            ImageDisplayState::LoadingSpinner
        } else if has_protocol {
            ImageDisplayState::Ready
        } else if let Some(err) = error_message {
            ImageDisplayState::Error(err)
        } else {
            ImageDisplayState::Empty
        }
    }

    /// Constructs a self-contained `UiModel` presentation snapshot for the active frame.
    pub fn ui_model(&mut self) -> crate::ui::UiModel<'_> {
        let display_state = Self::compute_display_state(
            self.image_protocol.is_some(),
            self.thumbnail_image.is_some(),
            self.initial_load
                .should_show_spinner(Duration::from_millis(150)),
            self.error_message.as_deref(),
        );
        let is_loading = self.is_loading();
        let rendered_size_cells = self.rendered_size_cells;
        let infobar_position = self.infobar;

        let infobar = if infobar_position == InfoBarPosition::None {
            None
        } else if self.queue.is_empty() {
            Some(crate::ui::InfoBarModel::Empty)
        } else {
            let classification = self.current_classification();
            let relative_path = self.queue.current_relative_path(self.view_mode);
            let view_mode_name = self.view_mode_name();
            let visible_position_1based =
                self.get_visible_position().map(|pos| pos + 1).unwrap_or(0);
            let visible_count = self.get_visible_count();
            let is_raw = self
                .queue
                .images()
                .get(self.queue.current_index)
                .map(|src| src.is_raw())
                .unwrap_or(false);

            Some(crate::ui::InfoBarModel::Active {
                classification,
                relative_path,
                view_mode_name,
                visible_position_1based,
                visible_count,
                img_width: self.img_width,
                img_height: self.img_height,
                show_thumbnail_only: self.show_thumbnail_only,
                is_raw,
                scale_mode: self.scale_mode,
                filter_name: self.filter_name(),
                current_zoom_pct: self.current_zoom_pct,
                pan_offset: self.pan_offset,
                brightness: self.current_brightness(),
                contrast: self.current_contrast(),
                slideshow_state: self.slideshow_state,
            })
        };

        let overlay = match self.palette_mode {
            PaletteMode::Closed => {
                if self.slideshow_state.is_paused() {
                    crate::ui::OverlayModel::SlideshowPaused {
                        infobar: infobar_position,
                    }
                } else {
                    crate::ui::OverlayModel::None
                }
            }
            PaletteMode::Prompt(prompt_type) => {
                crate::ui::OverlayModel::Prompt(crate::ui::PromptViewModel {
                    prompt_type,
                    query: self.palette_query.value(),
                    cursor_byte_offset: self.palette_query.cursor_byte_offset(),
                    palette_height: self.palette_height,
                    infobar: infobar_position,
                })
            }
            PaletteMode::Info => crate::ui::OverlayModel::Info(if self.queue.is_empty() {
                crate::ui::InfoBoxModel::Empty {
                    palette_height: self.palette_height,
                    infobar: infobar_position,
                }
            } else {
                let source = &self.queue.images()[self.queue.current_index];
                let uncompressed_mem_bytes = self
                    .original_image
                    .as_ref()
                    .map(|img| img.as_bytes().len() as u64)
                    .unwrap_or(0);
                let rotation_degrees = self
                    .queue
                    .adjustment(self.queue.current_index)
                    .rotation
                    .to_degrees();

                crate::ui::InfoBoxModel::Populated {
                    palette_height: self.palette_height,
                    infobar: infobar_position,
                    source,
                    uncompressed_mem_bytes,
                    img_width: self.img_width,
                    img_height: self.img_height,
                    classification: self.current_classification(),
                    brightness: self.current_brightness(),
                    contrast: self.current_contrast(),
                    rotation_degrees,
                    stats: &self.stats,
                    disable_thumbnail: self.disable_thumbnail,
                    show_thumbnail_only: self.show_thumbnail_only,
                    prefetch_cache_stats: self.prefetch_cache_stats(),
                    filter_name: self.filter_name(),
                    current_zoom_pct: self.current_zoom_pct,
                    active_protocol: self.active_protocol,
                }
            }),
            PaletteMode::File => {
                let filtered_files = &self.filtered_files[..];
                let relative_paths = self.queue.relative_paths();
                let classifications = self.queue.classifications();

                crate::ui::OverlayModel::Palette(crate::ui::PaletteViewModel {
                    kind: crate::ui::PaletteListKind::File {
                        filtered_files,
                        relative_paths,
                        classifications,
                    },
                    query: self.palette_query.value(),
                    cursor_byte_offset: self.palette_query.cursor_byte_offset(),
                    selected_index: self.palette_selected_index,
                    palette_width: self.palette_width,
                    palette_height: self.palette_height,
                    infobar: infobar_position,
                })
            }
            PaletteMode::Command => {
                let filtered_commands = &self.filtered_commands[..];

                crate::ui::OverlayModel::Palette(crate::ui::PaletteViewModel {
                    kind: crate::ui::PaletteListKind::Command { filtered_commands },
                    query: self.palette_query.value(),
                    cursor_byte_offset: self.palette_query.cursor_byte_offset(),
                    selected_index: self.palette_selected_index,
                    palette_width: self.palette_width,
                    palette_height: self.palette_height,
                    infobar: infobar_position,
                })
            }
        };

        crate::ui::UiModel {
            infobar_position,
            viewport: crate::ui::ViewportModel {
                display_state,
                is_loading,
                rendered_size_cells,
                image_protocol: self.image_protocol.as_mut(),
            },
            infobar,
            overlay,
        }
    }

    /// Returns `true` if active image loading is in progress.
    #[inline]
    pub fn is_loading(&self) -> bool {
        self.initial_load.is_active()
    }

    /// Returns `true` if the viewport is dirty and requires a resize request dispatch.
    #[inline]
    pub fn needs_update(&self) -> bool {
        self.resize_state.is_dirty()
    }

    /// Returns `true` if an immediate screen clear is scheduled.
    #[inline]
    pub fn needs_clear(&self) -> bool {
        self.screen_clear.is_pending()
    }

    /// Returns `true` if neighbor prefetching is currently suspended due to rapid navigation.
    #[inline]
    pub fn is_prefetch_suspended(&self) -> bool {
        self.scroll_cadence.is_prefetch_suspended()
    }

    /// Sets an error state, terminating loading and clearing image protocol and buffer data.
    pub fn set_error_state(&mut self, msg: impl Into<String>) {
        self.error_message = Some(msg.into());
        self.initial_load = InitialLoadState::Idle;
        self.original_image = None;
        self.thumbnail_image = None;
        self.image_protocol = None;
        self.resize_state.reset_idle();
        self.load_phase = LoadPhase::Complete;
    }

    /// Returns the number of cached entries and their total uncompressed memory footprint in bytes.
    pub fn prefetch_cache_stats(&self) -> (usize, u64) {
        self.prefetch_cache.stats()
    }

    /// Returns cached filtered file indices matching the current search query.
    pub fn get_filtered_files(&self) -> &[usize] {
        &self.filtered_files
    }

    /// Returns cached filtered command indices matching the current search query.
    pub fn get_filtered_commands(&self) -> &[usize] {
        &self.filtered_commands
    }

    /// Re-calculates and caches the matched files or commands based on the query.
    pub fn update_palette_filter(&mut self) {
        match self.palette_mode {
            PaletteMode::File => {
                let view_mode = self.view_mode;
                let queue = &self.queue;
                palette::filter_files(
                    self.palette_query.value(),
                    queue.relative_paths(),
                    queue.relative_paths_lowercase(),
                    &mut self.matcher,
                    |idx| queue.is_visible(idx, view_mode),
                    &mut self.filtered_files,
                    &mut self.palette_scratch,
                );
                if !self.filtered_files.is_empty() {
                    self.palette_selected_index = self
                        .palette_selected_index
                        .min(self.filtered_files.len() - 1);
                } else {
                    self.palette_selected_index = 0;
                }
            }
            PaletteMode::Command => {
                palette::filter_commands(
                    self.palette_query.value(),
                    &mut self.matcher,
                    &mut self.filtered_commands,
                    &mut self.palette_scratch,
                );
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

    /// Closes any active palette overlay and shrinks palette arena buffers if they exceeded `MAX_IDLE_PALETTE_CAPACITY`.
    pub fn close_palette(&mut self) {
        self.palette_mode = PaletteMode::Closed;
        self.screen_clear.request_now();
        self.palette_scratch.clear();
        if self.palette_scratch.capacity() > palette::MAX_IDLE_PALETTE_CAPACITY {
            self.palette_scratch
                .shrink_to(palette::MAX_IDLE_PALETTE_CAPACITY);
        }
        self.filtered_files.clear();
        if self.filtered_files.capacity() > palette::MAX_IDLE_PALETTE_CAPACITY {
            self.filtered_files
                .shrink_to(palette::MAX_IDLE_PALETTE_CAPACITY);
        }
        self.filtered_commands.clear();
        if self.filtered_commands.capacity() > palette::MAX_IDLE_PALETTE_CAPACITY {
            self.filtered_commands
                .shrink_to(palette::MAX_IDLE_PALETTE_CAPACITY);
        }
    }

    /// Returns the human-readable string name of the active filter type.
    pub fn filter_name(&self) -> &'static str {
        match self.filter_type {
            FilterType::Nearest => "Nearest",
            FilterType::Box => "Box",
            FilterType::Linear => "Linear",
            FilterType::Mitchell => "Mitchell",
            FilterType::Lanczos => "Lanczos",
        }
    }

    /// Cycles through available image scaling filters.
    pub fn cycle_filter(&mut self) {
        if self.is_loading() {
            return;
        }
        self.filter_type = match self.filter_type {
            FilterType::Nearest => FilterType::Box,
            FilterType::Box => FilterType::Linear,
            FilterType::Linear => FilterType::Mitchell,
            FilterType::Mitchell => FilterType::Lanczos,
            FilterType::Lanczos => FilterType::Nearest,
        };
        self.resize_state.mark_dirty();
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
                self.resize_state.mark_dirty();
            }
            Command::SetFilterBox => {
                self.filter_type = FilterType::Box;
                self.resize_state.mark_dirty();
            }
            Command::SetFilterLinear => {
                self.filter_type = FilterType::Linear;
                self.resize_state.mark_dirty();
            }
            Command::SetFilterMitchell => {
                self.filter_type = FilterType::Mitchell;
                self.resize_state.mark_dirty();
            }
            Command::SetFilterLanczos => {
                self.filter_type = FilterType::Lanczos;
                self.resize_state.mark_dirty();
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
                    self.resize_state.mark_dirty();
                    self.screen_clear.defer_until_protocol();
                }
            }
            Command::ShowInfo => {
                if self.palette_mode == PaletteMode::Info {
                    if self
                        .last_info_toggle
                        .is_none_or(|t| t.elapsed() > std::time::Duration::from_millis(200))
                    {
                        self.close_palette();
                        self.last_info_toggle = Some(std::time::Instant::now());
                    }
                } else {
                    self.palette_mode = PaletteMode::Info;
                    self.palette_height = 23;
                    self.screen_clear.request_now();
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
                self.resize_state.mark_dirty();
                self.screen_clear.request_now();
            }
            Command::SetInfoBarBottom => {
                self.infobar = InfoBarPosition::Bottom;
                self.resize_state.mark_dirty();
                self.screen_clear.request_now();
            }
            Command::SetInfoBarNone => {
                self.infobar = InfoBarPosition::None;
                self.resize_state.mark_dirty();
                self.screen_clear.request_now();
            }
            Command::CycleInfoBar => {
                self.infobar = match self.infobar {
                    InfoBarPosition::Bottom => InfoBarPosition::Top,
                    InfoBarPosition::Top => InfoBarPosition::None,
                    InfoBarPosition::None => InfoBarPosition::Bottom,
                };
                self.resize_state.mark_dirty();
                self.screen_clear.request_now();
            }
            Command::ToggleSlideshowPause => {
                self.slideshow_state = match self.slideshow_state {
                    SlideshowState::Playing { delay } => SlideshowState::Paused { delay },
                    SlideshowState::Paused { delay } => SlideshowState::Playing { delay },
                    SlideshowState::Stopped => SlideshowState::Stopped,
                };
                self.screen_clear.request_now();
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
        self.screen_clear.request_now();
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
        self.palette_mode = PaletteMode::Prompt(prompt_type);
        self.palette_query.clear();
        self.palette_selected_index = 0;
        self.palette_width = 45;
        self.palette_height = 0;
        self.screen_clear.request_now();
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
                        self.resize_state.mark_dirty();
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
                        self.resize_state.mark_dirty();
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
        self.close_palette();
    }

    /// Writes all non-unflagged classifications (active queue and unmapped entries) in
    /// tab-separated `<PREFIX>\t<IDENTIFIER>` format to `writer`.
    pub fn write_classifications_tsv<W: std::io::Write>(
        &self,
        mut writer: W,
    ) -> std::io::Result<()> {
        for (idx, &class) in self.classifications().iter().enumerate() {
            if class != Classification::Unflagged {
                let img = &self.queue.images()[idx];
                writeln!(writer, "{}\t{}", class.export_prefix(), img.identifier())?;
            }
        }
        for (key, &(class, _)) in &self.unmapped_classifications {
            if class != Classification::Unflagged {
                writeln!(writer, "{}\t{}", class.export_prefix(), key)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
/// Helper to instantiate a test App with mock channels without spawning worker threads.
pub fn setup_test_app_with_channels() -> (App, MockChannels) {
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
        active_protocol: crate::config::ActiveProtocol::Halfblocks,
    };
    let scan_config = ScanConfig::Directory {
        path: std::path::PathBuf::from("."),
        check_magic: false,
        recursive: false,
    };
    App::new_mocked(images, 0, picker, app_config, scan_config).unwrap()
}

#[cfg(test)]
/// Helper to instantiate a test App without spawning worker threads.
pub fn setup_test_app() -> App {
    let (app, _) = setup_test_app_with_channels();
    app
}
