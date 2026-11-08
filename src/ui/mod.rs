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
    use crate::app::{PaletteMode, PromptType, setup_test_app};
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

    #[test]
    fn test_infobar_positions() {
        let mut app = setup_test_app();
        app.img_width = 800;
        app.img_height = 600;

        // 1. Bottom infobar
        app.infobar = InfoBarPosition::Bottom;
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal
            .draw(|f| {
                let mut model = app.ui_model();
                ui(f, &mut model);
            })
            .unwrap();
        let rendered = buffer_to_string(&terminal);
        assert!(rendered.contains("img1.png"));
        assert!(rendered.contains("[1/2]"));
        assert!(rendered.contains("Zoom: 100%"));

        // 2. Top infobar
        app.infobar = InfoBarPosition::Top;
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal
            .draw(|f| {
                let mut model = app.ui_model();
                ui(f, &mut model);
            })
            .unwrap();
        let rendered = buffer_to_string(&terminal);
        assert!(rendered.contains("img1.png"));
        assert!(rendered.contains("[1/2]"));
        assert!(rendered.contains("Zoom: 100%"));

        // 3. Off (None) infobar
        app.infobar = InfoBarPosition::None;
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal
            .draw(|f| {
                let mut model = app.ui_model();
                ui(f, &mut model);
            })
            .unwrap();
        let rendered = buffer_to_string(&terminal);
        assert!(!rendered.contains("Zoom: 100%"));
        assert!(!rendered.contains("[1/2]"));
    }

    #[test]
    fn test_overlay_rendering() {
        let mut app = setup_test_app();
        app.img_width = 800;
        app.img_height = 600;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();

        // 1. Command Palette overlay (show_help)
        app.open_palette(PaletteMode::Command);
        terminal
            .draw(|f| {
                let mut model = app.ui_model();
                ui(f, &mut model);
            })
            .unwrap();
        let rendered = buffer_to_string(&terminal);
        assert!(rendered.contains("Command Palette"));

        // 2. File search overlay
        app.open_palette(PaletteMode::File);
        terminal
            .draw(|f| {
                let mut model = app.ui_model();
                ui(f, &mut model);
            })
            .unwrap();
        let rendered = buffer_to_string(&terminal);
        assert!(rendered.contains("File Search"));

        // 3. Goto image prompt overlay
        app.open_prompt(PromptType::GoToImage);
        terminal
            .draw(|f| {
                let mut model = app.ui_model();
                ui(f, &mut model);
            })
            .unwrap();
        let rendered = buffer_to_string(&terminal);
        assert!(rendered.contains("Go to Image"));

        // 4. Stats / Info dialog overlay (show_stats)
        let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
        app.palette_mode = PaletteMode::Info;
        app.palette_height = 23;
        terminal
            .draw(|f| {
                let mut model = app.ui_model();
                ui(f, &mut model);
            })
            .unwrap();
        let rendered = buffer_to_string(&terminal);
        assert!(rendered.contains("Image Details"));
        assert!(rendered.contains("Halfblocks"));
        assert!(rendered.contains("Protocol Encode:"));
        assert!(rendered.contains("Prefetch Cache:"));
        assert!(rendered.contains("Total Pipeline:"));

        // Test Kitty compression and SHM display
        app.active_protocol =
            crate::config::ActiveProtocol::Kitty(crate::config::KittyTransferMode::Compressed);
        terminal
            .draw(|f| {
                let mut model = app.ui_model();
                ui(f, &mut model);
            })
            .unwrap();
        let rendered_kitty_on = buffer_to_string(&terminal);
        assert!(rendered_kitty_on.contains("Kitty (Compressed)"));

        app.active_protocol =
            crate::config::ActiveProtocol::Kitty(crate::config::KittyTransferMode::Direct);
        terminal
            .draw(|f| {
                let mut model = app.ui_model();
                ui(f, &mut model);
            })
            .unwrap();
        let rendered_kitty_off = buffer_to_string(&terminal);
        assert!(rendered_kitty_off.contains("Kitty"));
        assert!(!rendered_kitty_off.contains("Compressed"));
        assert!(!rendered_kitty_off.contains("(SHM)"));

        app.active_protocol =
            crate::config::ActiveProtocol::Kitty(crate::config::KittyTransferMode::SharedMemory);
        terminal
            .draw(|f| {
                let mut model = app.ui_model();
                ui(f, &mut model);
            })
            .unwrap();
        let rendered_kitty_shm = buffer_to_string(&terminal);
        assert!(rendered_kitty_shm.contains("Kitty (SHM)"));
    }

    #[test]
    fn test_loading_spinner_debounce() {
        let mut app = setup_test_app();
        app.img_width = 800;
        app.img_height = 600;
        app.image_protocol = None;
        app.thumbnail_image = None;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();

        // 1. Loading started immediately (< 150ms debounce) -> "Loading Image..." must NOT be rendered
        app.initial_load = crate::app::InitialLoadState::start();
        terminal
            .draw(|f| {
                let mut model = app.ui_model();
                ui(f, &mut model);
            })
            .unwrap();
        let rendered_early = buffer_to_string(&terminal);
        assert!(
            !rendered_early.contains("Loading Image..."),
            "Loading text should be debounced under 150ms"
        );

        // 2. Loading exceeded 150ms (simulated 200ms ago) -> "Loading Image..." IS rendered
        app.initial_load = crate::app::InitialLoadState::start_at(
            std::time::Instant::now() - std::time::Duration::from_millis(200),
        );
        terminal
            .draw(|f| {
                let mut model = app.ui_model();
                ui(f, &mut model);
            })
            .unwrap();
        let rendered_late = buffer_to_string(&terminal);
        assert!(
            rendered_late.contains("Loading Image..."),
            "Loading text should be rendered when loading exceeds 150ms"
        );

        // 3. When thumbnail_image is Some, "Loading Image..." is NOT rendered even after 200ms
        app.thumbnail_image = Some(std::sync::Arc::new(image::DynamicImage::ImageRgb8(
            image::RgbImage::new(100, 100),
        )));
        terminal
            .draw(|f| {
                let mut model = app.ui_model();
                ui(f, &mut model);
            })
            .unwrap();
        let rendered_thumb = buffer_to_string(&terminal);
        assert!(
            !rendered_thumb.contains("Loading Image..."),
            "Loading text should not be rendered if thumbnail is available"
        );
    }

    #[test]
    fn test_tiny_terminal_rendering_defensive_clamping() {
        let mut app = setup_test_app();
        app.img_width = 800;
        app.img_height = 600;

        let modes = [
            PaletteMode::Closed,
            PaletteMode::Command,
            PaletteMode::File,
            PaletteMode::Prompt(PromptType::GoToImage),
            PaletteMode::Info,
        ];

        let sizes = [(10, 4), (1, 1), (0, 0)];

        for &(width, height) in &sizes {
            for mode in &modes {
                app.palette_mode = *mode;
                app.palette_width = width;
                app.palette_height = height;

                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                let res = terminal.draw(|f| {
                    let mut model = app.ui_model();
                    ui(f, &mut model);
                });
                assert!(
                    res.is_ok(),
                    "Failed rendering mode {:?} in {}x{} terminal",
                    mode,
                    width,
                    height
                );
            }

            // Also verify paused slideshow overlay defensively renders without panics
            app.palette_mode = PaletteMode::Closed;
            app.slideshow_state = crate::config::SlideshowState::Paused {
                delay: std::time::Duration::from_secs(5),
            };
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let res = terminal.draw(|f| {
                let mut model = app.ui_model();
                ui(f, &mut model);
            });
            assert!(
                res.is_ok(),
                "Failed rendering paused slideshow in {}x{} terminal",
                width,
                height
            );
        }
    }

    #[test]
    fn test_ui_model_standalone_rendering() {
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        let mut model = UiModel {
            infobar_position: InfoBarPosition::Bottom,
            viewport: ViewportModel {
                display_state: ImageDisplayState::Empty,
                is_loading: false,
                rendered_size_cells: (0, 0),
                image_protocol: None,
            },
            infobar: Some(InfoBarModel::Active {
                classification: crate::app::Classification::Pick,
                relative_path: "test.png",
                view_mode_name: "All",
                visible_position_1based: 1,
                visible_count: 1,
                img_width: 800,
                img_height: 600,
                show_thumbnail_only: false,
                is_raw: false,
                scale_mode: crate::imaging::ScaleMode::Shrink,
                filter_name: "Triangle",
                current_zoom_pct: 100.0,
                pan_offset: crate::imaging::PanOffset::ZERO,
                brightness: crate::imaging::Brightness::ZERO,
                contrast: crate::imaging::Contrast::ZERO,
                slideshow_state: crate::config::SlideshowState::Stopped,
            }),
            overlay: OverlayModel::None,
        };

        terminal.draw(|f| ui(f, &mut model)).unwrap();
        let rendered = buffer_to_string(&terminal);
        assert!(rendered.contains("test.png"));
        assert!(rendered.contains("[1/1]"));
        assert!(rendered.contains("Zoom: 100%"));
    }
}
