use crate::app::App;
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Style},
    widgets::{Block, BorderType, Borders, Paragraph},
};
use ratatui_image::StatefulImage;
use std::time::Duration;

/// Renders the main image viewport, handling loading spinners, error details, and empty states.
pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let show_loading = app.is_loading
        && (app.image_protocol.is_none()
            || (app.thumbnail_image.is_none()
                && app
                    .loading_start_time
                    .is_some_and(|t| t.elapsed() > Duration::from_millis(150))));

    if show_loading {
        let loading_paragraph = Paragraph::new("\n\nLoading Image...")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::Yellow).bold());
        frame.render_widget(loading_paragraph, area);
    } else if let Some(ref mut protocol) = app.image_protocol {
        // Calculate the centered Rect inside area
        let (rect_w, rect_h) = app.rendered_size_cells;
        let rect_w = rect_w.min(area.width);
        let rect_h = rect_h.min(area.height);
        let x = area.x + (area.width.saturating_sub(rect_w)) / 2;
        let y = area.y + (area.height.saturating_sub(rect_h)) / 2;
        let centered_rect = Rect::new(x, y, rect_w, rect_h);

        let image_widget = StatefulImage::default();
        frame.render_stateful_widget(image_widget, centered_rect, protocol);
    } else if let Some(ref err) = app.error_message {
        let err_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title(" Error Loading Image ")
            .style(Style::default().fg(Color::Red));
        let err_paragraph = Paragraph::new(err.as_str())
            .block(err_block)
            .alignment(Alignment::Center);
        frame.render_widget(err_paragraph, area);
    } else {
        let loading_paragraph = Paragraph::new("No image loaded.")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::Yellow));
        frame.render_widget(loading_paragraph, area);
    }
}
