pub mod views;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
};

use crate::app::{App, PaletteMode};

/// Renders the entire view layout: including centered images, bottom status HUD,
/// and interactive command/file palette overlays.
pub fn ui(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(frame.area());

    // Render image viewport
    views::image_view::draw(frame, app, chunks[0]);

    // Render bottom status hud
    views::status_hud::draw(frame, app, chunks[1]);

    // Render active overlay widgets
    if app.palette_mode != PaletteMode::Closed {
        match app.palette_mode {
            PaletteMode::Prompt => {
                views::prompt::draw(frame, app, chunks[0]);
            }
            PaletteMode::Info => {
                views::info_box::draw(frame, app, chunks[0]);
            }
            PaletteMode::Command | PaletteMode::File => {
                views::palette::draw(frame, app, chunks[0]);
            }
            _ => {}
        }
    }
}
