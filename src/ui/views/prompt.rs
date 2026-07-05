use crate::app::{App, PromptType};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style, Stylize},
    text::Line,
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

/// Renders the confirm or value input prompt box (for delays, indexes, biases).
pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let prompt_title = match app.prompt_type {
        Some(PromptType::GoToImage) => " Go to Image ",
        Some(PromptType::SetBrightness) => " Set Brightness ",
        Some(PromptType::SetContrast) => " Set Contrast ",
        Some(PromptType::SetSlideshow) => " Set Slideshow ",
        None => " Input ",
    };
    let prompt_label = match app.prompt_type {
        Some(PromptType::GoToImage) => "Enter index (e.g. 40, +10, -10):",
        Some(PromptType::SetBrightness) => "Enter brightness (e.g. 50, +10, -10):",
        Some(PromptType::SetContrast) => "Enter contrast % (e.g. 20, +5, -5):",
        Some(PromptType::SetSlideshow) => "Enter slideshow delay in seconds (e.g. 5, +1, -1):",
        None => "Enter value:",
    };

    let lines = vec![
        Line::from(format!("   {}", prompt_label).gray()),
        Line::from(vec![
            " > ".bold().cyan(),
            app.palette_query.as_str().into(),
            "▊".cyan(), // cursor block
        ]),
    ];

    let w = 45.min(area.width.saturating_sub(1));
    let h = app.palette_height.min(area.height.saturating_sub(1));

    let palette_block = Block::default()
        .title(prompt_title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .title_style(Style::default().fg(Color::Yellow).bold());

    let palette_paragraph = Paragraph::new(lines)
        .block(palette_block)
        .style(Style::default().fg(Color::White).bg(Color::Reset));

    let x = area.x + area.width.saturating_sub(w).saturating_sub(1);
    let y = area.y.saturating_add(1);

    let popup_area = Rect::new(x, y, w, h);
    frame.render_widget(Clear, popup_area);
    frame.render_widget(palette_paragraph, popup_area);
}
