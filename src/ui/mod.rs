/// Pure presentation view models decoupled from application state.
pub mod model;
/// Presentation submodules for overlay and layout widgets.
pub mod views;

pub use model::*;

use crate::config::InfoBarPosition;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
};

/// Renders the entire view layout: including centered images, bottom status HUD,
/// and interactive command/file palette overlays.
pub fn ui(frame: &mut Frame, model: &mut UiModel<'_>) {
    let (viewport_area, status_area) = match model.infobar_position {
        InfoBarPosition::None => (frame.area(), None),
        InfoBarPosition::Top => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(model.infobar_position.height()),
                    Constraint::Min(0),
                ])
                .split(frame.area());
            (chunks[1], Some(chunks[0]))
        }
        InfoBarPosition::Bottom => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(0),
                    Constraint::Length(model.infobar_position.height()),
                ])
                .split(frame.area());
            (chunks[0], Some(chunks[1]))
        }
    };

    // Render image viewport
    views::image_view::draw(frame, &mut model.viewport, viewport_area);

    // Render infobar if configured
    if let (Some(s_area), Some(info_model)) = (status_area, &model.infobar) {
        views::infobar::draw(frame, info_model, s_area);
    }

    // Render active overlay widgets
    match &model.overlay {
        OverlayModel::None => {}
        OverlayModel::Prompt(prompt_model) => {
            views::prompt::draw(frame, prompt_model, viewport_area);
        }
        OverlayModel::Info(info_model) => {
            views::info_box::draw(frame, info_model, viewport_area);
        }
        OverlayModel::Palette(palette_model) => {
            views::palette::draw(frame, palette_model, viewport_area);
        }
        OverlayModel::SlideshowPaused { infobar } => {
            views::slideshow_paused::draw(frame, *infobar, viewport_area);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::PromptType;
    use crate::config::{
        ActiveProtocol, Classification, InfoBarPosition, KittyTransferMode, SlideshowState,
    };
    use crate::imaging::{Brightness, Contrast, PanOffset, ScaleMode};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn buffer_to_string(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        let mut s = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                s.push_str(buffer[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }

    fn sample_ui_model<'a>() -> UiModel<'a> {
        UiModel {
            infobar_position: InfoBarPosition::Bottom,
            viewport: ViewportModel {
                display_state: ImageDisplayState::Empty,
                is_loading: false,
                rendered_size_cells: (0, 0),
                image_protocol: None,
            },
            infobar: Some(InfoBarModel::Active {
                classification: Classification::Pick,
                relative_path: "test.png",
                view_mode_name: "All Files",
                visible_position_1based: 1,
                visible_count: 2,
                img_width: 800,
                img_height: 600,
                show_thumbnail_only: false,
                is_raw: false,
                scale_mode: ScaleMode::Shrink,
                filter_name: "Triangle",
                current_zoom_pct: 100.0,
                pan_offset: PanOffset::ZERO,
                brightness: Brightness::ZERO,
                contrast: Contrast::ZERO,
                slideshow_state: SlideshowState::Stopped,
            }),
            overlay: OverlayModel::None,
        }
    }

    #[test]
    fn test_ui_model_infobar_positions() {
        // 1. Bottom infobar
        let mut model = sample_ui_model();
        model.infobar_position = InfoBarPosition::Bottom;
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal.draw(|f| ui(f, &mut model)).unwrap();
        let rendered = buffer_to_string(&terminal);
        assert!(rendered.contains("test.png"));
        assert!(rendered.contains("[1/2]"));
        assert!(rendered.contains("Zoom: 100%"));

        // 2. Top infobar
        let mut model = sample_ui_model();
        model.infobar_position = InfoBarPosition::Top;
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal.draw(|f| ui(f, &mut model)).unwrap();
        let rendered = buffer_to_string(&terminal);
        assert!(rendered.contains("test.png"));
        assert!(rendered.contains("[1/2]"));
        assert!(rendered.contains("Zoom: 100%"));

        // 3. Off (None) infobar
        let mut model = sample_ui_model();
        model.infobar_position = InfoBarPosition::None;
        model.infobar = None;
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal.draw(|f| ui(f, &mut model)).unwrap();
        let rendered = buffer_to_string(&terminal);
        assert!(!rendered.contains("Zoom: 100%"));
        assert!(!rendered.contains("[1/2]"));
    }

    #[test]
    fn test_ui_model_overlays() {
        // 1. Command Palette overlay
        let mut model = sample_ui_model();
        let filtered_commands = [0, 1];
        model.overlay = OverlayModel::Palette(PaletteViewModel {
            kind: PaletteListKind::Command {
                filtered_commands: &filtered_commands,
            },
            query: "quit",
            cursor_byte_offset: 4,
            selected_index: 0,
            palette_width: 50,
            palette_height: 20,
            infobar: InfoBarPosition::Bottom,
        });
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| ui(f, &mut model)).unwrap();
        let rendered = buffer_to_string(&terminal);
        assert!(rendered.contains("Command Palette"));

        // 2. File search overlay
        let mut model = sample_ui_model();
        let filtered_files = [0];
        let relative_paths = ["photo1.jpg".to_string()];
        let classifications = [Classification::Pick];
        model.overlay = OverlayModel::Palette(PaletteViewModel {
            kind: PaletteListKind::File {
                filtered_files: &filtered_files,
                relative_paths: &relative_paths,
                classifications: &classifications,
            },
            query: "photo",
            cursor_byte_offset: 5,
            selected_index: 0,
            palette_width: 50,
            palette_height: 20,
            infobar: InfoBarPosition::Bottom,
        });
        terminal.draw(|f| ui(f, &mut model)).unwrap();
        let rendered = buffer_to_string(&terminal);
        assert!(rendered.contains("File Search"));

        // 3. Goto image prompt overlay
        let mut model = sample_ui_model();
        model.overlay = OverlayModel::Prompt(PromptViewModel {
            prompt_type: PromptType::GoToImage,
            query: "42",
            cursor_byte_offset: 2,
            palette_height: 5,
            infobar: InfoBarPosition::Bottom,
        });
        terminal.draw(|f| ui(f, &mut model)).unwrap();
        let rendered = buffer_to_string(&terminal);
        assert!(rendered.contains("Go to Image"));

        // 4. Stats / Info dialog overlay
        let mut model = sample_ui_model();
        let source = crate::imaging::ImageSource::Local(std::path::PathBuf::from("photo.jpg"));
        let stats = StatsForNerds::default();
        model.overlay = OverlayModel::Info(InfoBoxModel::Populated {
            palette_height: 23,
            infobar: InfoBarPosition::Bottom,
            source: &source,
            uncompressed_mem_bytes: 1024 * 1024,
            img_width: 800,
            img_height: 600,
            classification: Classification::Pick,
            brightness: Brightness::ZERO,
            contrast: Contrast::ZERO,
            rotation_degrees: 0,
            stats: &stats,
            disable_thumbnail: false,
            show_thumbnail_only: false,
            prefetch_cache_stats: (1, 1024),
            filter_name: "Triangle",
            current_zoom_pct: 100.0,
            active_protocol: ActiveProtocol::Halfblocks,
        });
        let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
        terminal.draw(|f| ui(f, &mut model)).unwrap();
        let rendered = buffer_to_string(&terminal);
        assert!(rendered.contains("Image Details"));
        assert!(rendered.contains("Halfblocks"));
        assert!(rendered.contains("Protocol Encode:"));
        assert!(rendered.contains("Prefetch Cache:"));
        assert!(rendered.contains("Total Pipeline:"));

        // Test Kitty compression and SHM display
        if let OverlayModel::Info(InfoBoxModel::Populated {
            ref mut active_protocol,
            ..
        }) = model.overlay
        {
            *active_protocol = ActiveProtocol::Kitty(KittyTransferMode::Compressed);
        }
        terminal.draw(|f| ui(f, &mut model)).unwrap();
        let rendered_kitty_on = buffer_to_string(&terminal);
        assert!(rendered_kitty_on.contains("Kitty (Compressed)"));

        if let OverlayModel::Info(InfoBoxModel::Populated {
            ref mut active_protocol,
            ..
        }) = model.overlay
        {
            *active_protocol = ActiveProtocol::Kitty(KittyTransferMode::Direct);
        }
        terminal.draw(|f| ui(f, &mut model)).unwrap();
        let rendered_kitty_off = buffer_to_string(&terminal);
        assert!(rendered_kitty_off.contains("Kitty"));
        assert!(!rendered_kitty_off.contains("Compressed"));
        assert!(!rendered_kitty_off.contains("(SHM)"));

        if let OverlayModel::Info(InfoBoxModel::Populated {
            ref mut active_protocol,
            ..
        }) = model.overlay
        {
            *active_protocol = ActiveProtocol::Kitty(KittyTransferMode::SharedMemory);
        }
        terminal.draw(|f| ui(f, &mut model)).unwrap();
        let rendered_kitty_shm = buffer_to_string(&terminal);
        assert!(rendered_kitty_shm.contains("Kitty (SHM)"));

        // 5. SlideshowPaused overlay
        let mut model = sample_ui_model();
        model.overlay = OverlayModel::SlideshowPaused {
            infobar: InfoBarPosition::Bottom,
        };
        terminal.draw(|f| ui(f, &mut model)).unwrap();
        let rendered_paused = buffer_to_string(&terminal);
        assert!(rendered_paused.contains("Slideshow Paused"));
        assert!(rendered_paused.contains("Press Space to resume"));
    }

    #[test]
    fn test_ui_model_display_states() {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();

        // 1. LoadingSpinner
        let mut model = sample_ui_model();
        model.viewport.display_state = ImageDisplayState::LoadingSpinner;
        terminal.draw(|f| ui(f, &mut model)).unwrap();
        let rendered = buffer_to_string(&terminal);
        assert!(rendered.contains("Loading Image..."));

        // 2. Error
        let mut model = sample_ui_model();
        model.viewport.display_state = ImageDisplayState::Error("Decode failed: corrupt file");
        terminal.draw(|f| ui(f, &mut model)).unwrap();
        let rendered = buffer_to_string(&terminal);
        assert!(rendered.contains("Error Loading Image"));
        assert!(rendered.contains("Decode failed: corrupt file"));

        // 3. Empty
        let mut model = sample_ui_model();
        model.viewport.display_state = ImageDisplayState::Empty;
        terminal.draw(|f| ui(f, &mut model)).unwrap();
        let rendered = buffer_to_string(&terminal);
        assert!(!rendered.contains("Loading Image..."));
        assert!(!rendered.contains("Error:"));

        // 4. Ready
        let mut model = sample_ui_model();
        model.viewport.display_state = ImageDisplayState::Ready;
        terminal.draw(|f| ui(f, &mut model)).unwrap();
        let rendered = buffer_to_string(&terminal);
        assert!(!rendered.contains("Loading Image..."));
    }

    #[test]
    fn test_ui_model_tiny_terminal_clamping() {
        let sizes = [(10, 4), (1, 1), (0, 0)];

        for &(width, height) in &sizes {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

            // None
            let mut model = sample_ui_model();
            model.overlay = OverlayModel::None;
            assert!(terminal.draw(|f| ui(f, &mut model)).is_ok());

            // SlideshowPaused
            let mut model = sample_ui_model();
            model.overlay = OverlayModel::SlideshowPaused {
                infobar: InfoBarPosition::Bottom,
            };
            assert!(terminal.draw(|f| ui(f, &mut model)).is_ok());

            // Prompt
            let mut model = sample_ui_model();
            model.overlay = OverlayModel::Prompt(PromptViewModel {
                prompt_type: PromptType::GoToImage,
                query: "1",
                cursor_byte_offset: 1,
                palette_height: height,
                infobar: InfoBarPosition::Bottom,
            });
            assert!(terminal.draw(|f| ui(f, &mut model)).is_ok());

            // Info Empty
            let mut model = sample_ui_model();
            model.overlay = OverlayModel::Info(InfoBoxModel::Empty {
                palette_height: height,
                infobar: InfoBarPosition::Bottom,
            });
            assert!(terminal.draw(|f| ui(f, &mut model)).is_ok());
        }
    }
}
