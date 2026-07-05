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
        InfoBarPosition::None => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(1)])
                .split(frame.area());
            (chunks[0], None)
        }
        InfoBarPosition::Top => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(0),
                    Constraint::Length(1),
                ])
                .split(frame.area());
            (chunks[1], Some(chunks[0]))
        }
        InfoBarPosition::Bottom => {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(3)])
                .split(frame.area());
            (chunks[0], Some(chunks[1]))
        }
    };

    // Render image viewport
    views::image_view::draw(frame, app, viewport_area);

    // Render status hud if configured
    if let Some(s_area) = status_area {
        views::status_hud::draw(frame, app, s_area);
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
        use ratatui::style::{Color, Style, Stylize};
        use ratatui::text::Line;
        use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

        let w = 34.min(viewport_area.width.saturating_sub(1));
        let h = 3.min(viewport_area.height.saturating_sub(1));

        let block = Block::default()
            .title(" Slideshow Paused ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Cyan))
            .title_style(Style::default().fg(Color::Yellow).bold());

        let lines = vec![Line::from("   Press Space to resume.".bold().white())];

        let paragraph = Paragraph::new(lines)
            .block(block)
            .style(Style::default().fg(Color::White).bg(Color::Reset));

        let x = viewport_area.x + viewport_area.width.saturating_sub(w).saturating_sub(1);
        let y = if app.infobar == InfoBarPosition::Top {
            viewport_area.y
        } else {
            viewport_area.y.saturating_add(1)
        };

        let popup_area = ratatui::layout::Rect::new(x, y, w, h);
        frame.render_widget(Clear, popup_area);
        frame.render_widget(paragraph, popup_area);
    }
}
