use crate::app::App;
use crate::config::InfoBarPosition;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style, Stylize},
    text::Line,
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

/// Renders a pop-up dialog indicating that the slideshow is paused.
pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let w = 34.min(area.width.saturating_sub(1));
    let h = 3.min(area.height.saturating_sub(1));

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

    let x = area.x + area.width.saturating_sub(w).saturating_sub(1);
    let y = if app.infobar == InfoBarPosition::Top {
        area.y
    } else {
        area.y.saturating_add(1)
    };

    let popup_area = Rect::new(x, y, w, h);
    frame.render_widget(Clear, popup_area);
    frame.render_widget(paragraph, popup_area);
}
