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
