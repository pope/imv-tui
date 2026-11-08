use crate::config::InfoBarPosition;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style, Stylize},
    text::Line,
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

/// Renders a pop-up dialog indicating that the slideshow is paused.
pub fn draw(frame: &mut Frame, infobar: InfoBarPosition, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

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

    let popup_area = super::top_right_popup_rect(area, 34, 3, infobar);
    frame.render_widget(Clear, popup_area);
    frame.render_widget(paragraph, popup_area);
}
