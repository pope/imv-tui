/// Presentation submodules for overlay and layout widgets.
pub mod views;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
};

use crate::app::{App, PaletteMode};
use crate::config::InfoBarPosition;

/// Renders the entire view layout: including centered images, bottom status HUD,
/// and interactive command/file palette overlays.
pub fn ui(frame: &mut Frame, app: &mut App) {
    let (viewport_area, status_area) = match app.infobar {
        InfoBarPosition::None => (frame.area(), None),
        InfoBarPosition::Top => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(app.infobar.height()), Constraint::Min(0)])
                .split(frame.area());
            (chunks[1], Some(chunks[0]))
        }
        InfoBarPosition::Bottom => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(app.infobar.height())])
                .split(frame.area());
            (chunks[0], Some(chunks[1]))
        }
    };

    // Render image viewport
    views::image_view::draw(frame, app, viewport_area);

    // Render infobar if configured
    if let Some(s_area) = status_area {
        views::infobar::draw(frame, app, s_area);
    }

    // Render active overlay widgets
    if app.palette_mode != PaletteMode::Closed {
        match app.palette_mode {
            PaletteMode::Prompt => {
                views::prompt::draw(frame, app, viewport_area);
            }
            PaletteMode::Info => {
                views::info_box::draw(frame, app, viewport_area);
            }
            PaletteMode::Command | PaletteMode::File => {
                views::palette::draw(frame, app, viewport_area);
            }
            _ => {}
        }
    } else if app.slideshow_state.is_paused() {
        views::slideshow_paused::draw(frame, app, viewport_area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{PromptType, setup_test_app};
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
        terminal.draw(|f| ui(f, &mut app)).unwrap();
        let rendered = buffer_to_string(&terminal);
        assert!(rendered.contains("img1.png"));
        assert!(rendered.contains("[1/2]"));
        assert!(rendered.contains("Zoom: 100%"));

        // 2. Top infobar
        app.infobar = InfoBarPosition::Top;
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal.draw(|f| ui(f, &mut app)).unwrap();
        let rendered = buffer_to_string(&terminal);
        assert!(rendered.contains("img1.png"));
        assert!(rendered.contains("[1/2]"));
        assert!(rendered.contains("Zoom: 100%"));

        // 3. Off (None) infobar
        app.infobar = InfoBarPosition::None;
        let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
        terminal.draw(|f| ui(f, &mut app)).unwrap();
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
        terminal.draw(|f| ui(f, &mut app)).unwrap();
        let rendered = buffer_to_string(&terminal);
        assert!(rendered.contains("Command Palette"));

        // 2. File search overlay
        app.open_palette(PaletteMode::File);
        terminal.draw(|f| ui(f, &mut app)).unwrap();
        let rendered = buffer_to_string(&terminal);
        assert!(rendered.contains("File Search"));

        // 3. Goto image prompt overlay
        app.open_prompt(PromptType::GoToImage);
        terminal.draw(|f| ui(f, &mut app)).unwrap();
        let rendered = buffer_to_string(&terminal);
        assert!(rendered.contains("Go to Image"));

        // 4. Stats / Info dialog overlay (show_stats)
        app.palette_mode = PaletteMode::Info;
        app.palette_height = 19;
        terminal.draw(|f| ui(f, &mut app)).unwrap();
        let rendered = buffer_to_string(&terminal);
        assert!(rendered.contains("Image Details"));
    }

    #[test]
    fn test_tiny_terminal_rendering_defensive_clamping() {
        let mut app = setup_test_app();
        app.img_width = 800;
        app.img_height = 600;

        let modes = [
            (PaletteMode::Closed, None),
            (PaletteMode::Command, None),
            (PaletteMode::File, None),
            (PaletteMode::Prompt, Some(PromptType::GoToImage)),
            (PaletteMode::Info, None),
        ];

        let sizes = [(10, 4), (1, 1), (0, 0)];

        for &(width, height) in &sizes {
            for (mode, prompt) in &modes {
                app.palette_mode = *mode;
                app.prompt_type = *prompt;
                app.palette_width = width;
                app.palette_height = height;

                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                let res = terminal.draw(|f| ui(f, &mut app));
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
            app.prompt_type = None;
            app.slideshow_state = crate::config::SlideshowState::Paused {
                delay: std::time::Duration::from_secs(5),
            };
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let res = terminal.draw(|f| ui(f, &mut app));
            assert!(
                res.is_ok(),
                "Failed rendering paused slideshow in {}x{} terminal",
                width,
                height
            );
        }
    }
}
