use crate::commands::PromptType;
use crate::config::{ActiveProtocol, Classification, InfoBarPosition, SlideshowState};
use crate::imaging::{
    Brightness, Contrast, ImageSource, PanOffset, ScaleMode, ThumbnailSource, ViewportProtocol,
};

/// Telemetry metadata for a decoded thumbnail placeholder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThumbnailTelemetry {
    /// Time taken to extract and decode the thumbnail.
    pub load_duration: std::time::Duration,
    /// Dimensions of the decoded thumbnail placeholder (`(width, height)`).
    pub dimensions: (u32, u32),
    /// Source of the thumbnail placeholder (EXIF or XDG cache).
    pub source: ThumbnailSource,
}

/// Mutually-exclusive state of a thumbnail placeholder or telemetry entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThumbnailState<T> {
    /// No thumbnail was found (or none has been loaded yet).
    #[default]
    Absent,
    /// Thumbnail extraction was bypassed because the full image was decoded via background prefetch.
    Skipped,
    /// A thumbnail placeholder was extracted and decoded.
    Loaded(T),
}

impl<T> ThumbnailState<T> {
    /// Returns true if a thumbnail is loaded.
    pub const fn is_loaded(&self) -> bool {
        matches!(self, Self::Loaded(_))
    }

    /// Returns true if no thumbnail is present.
    pub const fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }

    /// Returns true if thumbnail extraction was skipped.
    pub const fn is_skipped(&self) -> bool {
        matches!(self, Self::Skipped)
    }

    /// Returns a reference to the loaded value, if any.
    pub const fn loaded(&self) -> Option<&T> {
        match self {
            Self::Loaded(v) => Some(v),
            _ => None,
        }
    }
}

/// Diagnostic and telemetry instrumentation for image decoding, resizing, and protocol delivery.
#[derive(Debug, Clone, Default)]
pub struct StatsForNerds {
    /// Time taken to load (decode) the photo from disk or zip, or None while loading.
    pub load_duration: Option<std::time::Duration>,
    /// Telemetry metadata for the decoded thumbnail placeholder, if any.
    pub thumbnail: ThumbnailState<ThumbnailTelemetry>,
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
    /// The original raw/sensor dimensions of the raw image (`(width, height)`), if any.
    pub raw_dimensions: Option<(u32, u32)>,
}

impl StatsForNerds {
    /// Computes the active foreground pipeline duration (`decode + resize + protocol encode`)
    /// for the currently displayed frame:
    /// - On a prefetch cache hit (`is_prefetch_cache_hit == true`), decode already completed
    ///   in the background prior to navigation (`0 ms` foreground decode wait), so only
    ///   `process_duration + protocol_duration` is counted.
    /// - When displaying a thumbnail placeholder (`show_thumbnail_only` or `load_duration.is_none()`),
    ///   uses the thumbnail's `load_duration` instead of `0 ms` or the full image's `load_duration`.
    pub fn total_pipeline_duration(&self, show_thumbnail_only: bool) -> std::time::Duration {
        let active_decode_dur = if self.is_prefetch_cache_hit {
            std::time::Duration::ZERO
        } else if (show_thumbnail_only || self.load_duration.is_none())
            && self.thumbnail.is_loaded()
        {
            self.thumbnail
                .loaded()
                .map(|t| t.load_duration)
                .unwrap_or_default()
        } else if show_thumbnail_only || self.load_duration.is_none() {
            std::time::Duration::ZERO
        } else {
            self.load_duration.unwrap_or_default()
        };
        active_decode_dur + self.process_duration + self.protocol_duration
    }
}

/// The mutually-exclusive display state for the image viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageDisplayState<'a> {
    /// Actively loading with debounce threshold elapsed and no preview available.
    LoadingSpinner,
    /// Image protocol is ready for display.
    Ready,
    /// An error occurred during loading.
    Error(&'a str),
    /// No image is currently loaded.
    Empty,
}

/// View model for the main image viewport (`src/ui/views/image_view.rs`).
pub struct ViewportModel<'a> {
    /// Current display state of the viewport.
    pub display_state: ImageDisplayState<'a>,
    /// Whether a background load is currently active.
    pub is_loading: bool,
    /// Rendered terminal cell dimensions `(cols, rows)` of the protocol image.
    pub rendered_size_cells: (u16, u16),
    /// Mutable reference to the active terminal graphics protocol, if any.
    pub image_protocol: Option<&'a mut ViewportProtocol>,
}

/// View model for the top/bottom status infobar (`src/ui/views/infobar.rs`).
pub enum InfoBarModel<'a> {
    /// Queue is empty.
    Empty,
    /// Active image metadata and viewport telemetry for the status bar.
    Active {
        /// Current image classification (`Unflagged`, `Pick`, `Reject`).
        classification: Classification,
        /// Relative path of the current image.
        relative_path: &'a str,
        /// Human-readable name of the active view mode.
        view_mode_name: &'static str,
        /// 1-based visible position in the filtered queue (or 0 if hidden).
        visible_position_1based: usize,
        /// Total number of visible images in the active view mode.
        visible_count: usize,
        /// Width of the current image in pixels.
        img_width: u32,
        /// Height of the current image in pixels.
        img_height: u32,
        /// Whether a low-res thumbnail placeholder is currently displayed.
        show_thumbnail_only: bool,
        /// Whether the current image source is a camera RAW file.
        is_raw: bool,
        /// Active scaling mode (`Fit`, `Crop`, etc.).
        scale_mode: ScaleMode,
        /// Active scaling filter name (`Lanczos3`, `Mitchell`, etc.).
        filter_name: &'static str,
        /// Current zoom percentage.
        current_zoom_pct: f64,
        /// Current pan offset `(x, y)`.
        pan_offset: PanOffset,
        /// Current brightness adjustment.
        brightness: Brightness,
        /// Current contrast adjustment.
        contrast: Contrast,
        /// Current slideshow state.
        slideshow_state: SlideshowState,
    },
}

/// View model for the "Image Details / Stats for Nerds" overlay (`src/ui/views/info_box.rs`).
pub enum InfoBoxModel<'a> {
    /// Queue is empty.
    Empty {
        /// Height of the popup in rows.
        palette_height: u16,
        /// Infobar position used for top-right anchoring.
        infobar: InfoBarPosition,
    },
    /// Populated image details and pipeline telemetry.
    Populated {
        /// Height of the popup in rows.
        palette_height: u16,
        /// Infobar position used for top-right anchoring.
        infobar: InfoBarPosition,
        /// Active image source (`Local` or `Cbz`).
        source: &'a ImageSource,
        /// Uncompressed memory footprint of the loaded image in bytes.
        uncompressed_mem_bytes: u64,
        /// Decoded image width in pixels.
        img_width: u32,
        /// Decoded image height in pixels.
        img_height: u32,
        /// Current classification flag.
        classification: Classification,
        /// Current brightness adjustment.
        brightness: Brightness,
        /// Current contrast adjustment.
        contrast: Contrast,
        /// Current rotation in degrees (`0`, `90`, `180`, `270`).
        rotation_degrees: u32,
        /// Pipeline and cache telemetry stats.
        stats: &'a StatsForNerds,
        /// Whether thumbnail extraction is disabled (`--no-thumbnail`).
        disable_thumbnail: bool,
        /// Whether the viewport is currently displaying the thumbnail placeholder.
        show_thumbnail_only: bool,
        /// Number of warm entries and total bytes in the prefetch cache.
        prefetch_cache_stats: (usize, u64),
        /// Active scaling filter name.
        filter_name: &'static str,
        /// Current zoom percentage.
        current_zoom_pct: f64,
        /// Active terminal graphics protocol and transfer mode.
        active_protocol: ActiveProtocol,
    },
}

/// Kind of searchable palette list (`File` or `Command`).
pub enum PaletteListKind<'a> {
    /// File fuzzy search list.
    File {
        /// Filtered queue indices matching the query.
        filtered_files: &'a [usize],
        /// Relative display paths of all queue items.
        relative_paths: &'a [String],
        /// Classifications of all queue items.
        classifications: &'a [Classification],
    },
    /// Command palette search list.
    Command {
        /// Filtered command registry indices matching the query.
        filtered_commands: &'a [usize],
    },
}

/// View model for the searchable command/file palette (`src/ui/views/palette.rs`).
pub struct PaletteViewModel<'a> {
    /// Whether rendering file search or command palette.
    pub kind: PaletteListKind<'a>,
    /// Current query string in the line editor.
    pub query: &'a str,
    /// Byte offset of the cursor within `query`.
    pub cursor_byte_offset: usize,
    /// Currently selected row index in the filtered list.
    pub selected_index: usize,
    /// Desired popup width in columns.
    pub palette_width: u16,
    /// Desired popup height in rows.
    pub palette_height: u16,
    /// Infobar position for top-right anchoring.
    pub infobar: InfoBarPosition,
}

/// View model for the single-line value prompt (`src/ui/views/prompt.rs`).
pub struct PromptViewModel<'a> {
    /// Prompt type (`GoToImage`, `SetBrightness`, `SetContrast`, `SetSlideshow`).
    pub prompt_type: PromptType,
    /// Current query string in the line editor.
    pub query: &'a str,
    /// Byte offset of the cursor within `query`.
    pub cursor_byte_offset: usize,
    /// Desired popup height in rows.
    pub palette_height: u16,
    /// Infobar position for top-right anchoring.
    pub infobar: InfoBarPosition,
}

/// Active overlay widget to render on top of the main viewport.
pub enum OverlayModel<'a> {
    /// No overlay is open.
    None,
    /// Single-line input prompt overlay.
    Prompt(PromptViewModel<'a>),
    /// Technical image details & telemetry overlay.
    Info(InfoBoxModel<'a>),
    /// Searchable command or file palette overlay.
    Palette(PaletteViewModel<'a>),
    /// Small "Slideshow Paused" indicator when no palette is open and slideshow is paused.
    SlideshowPaused {
        /// Infobar position for top-right anchoring.
        infobar: InfoBarPosition,
    },
}

/// Complete, self-contained presentation model for a single TUI frame draw.
pub struct UiModel<'a> {
    /// Configured infobar position (`Top`, `Bottom`, `None`).
    pub infobar_position: InfoBarPosition,
    /// Main image viewport model.
    pub viewport: ViewportModel<'a>,
    /// Optional status infobar model (`None` when `infobar_position == InfoBarPosition::None`).
    pub infobar: Option<InfoBarModel<'a>>,
    /// Active overlay model.
    pub overlay: OverlayModel<'a>,
}
