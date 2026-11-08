use crate::app::Classification;
use crate::commands::PromptType;
use crate::config::{ActiveProtocol, InfoBarPosition, SlideshowState};
use crate::imaging::{Brightness, Contrast, ImageSource, PanOffset, ScaleMode, ViewportProtocol};

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
        stats: &'a crate::app::StatsForNerds,
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
