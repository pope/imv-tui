use crate::imaging::ViewportProtocol;
use crate::ui::model::{ImageDisplayState, ViewportModel};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Style},
    widgets::{Block, BorderType, Borders, Paragraph, Widget},
};

/// Stateless widget adapter that renders a pre-encoded [`ViewportProtocol`] into a ratatui buffer.
struct ViewportImageWidget<'a>(&'a ViewportProtocol);

impl Widget for ViewportImageWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.0.render(area, buf);
    }
}

/// Renders the main image viewport, handling loading spinners, error details, and empty states.
pub fn draw(frame: &mut Frame, model: &mut ViewportModel<'_>, area: Rect) {
    match model.display_state {
        ImageDisplayState::LoadingSpinner => {
            let loading_paragraph = Paragraph::new("\n\nLoading Image...")
                .alignment(Alignment::Center)
                .style(Style::default().fg(Color::Yellow).bold());
            frame.render_widget(loading_paragraph, area);
        }
        ImageDisplayState::Ready => {
            if let Some(protocol) = model.image_protocol.as_deref() {
                // Calculate the centered Rect inside area
                let (rect_w, rect_h) = model.rendered_size_cells;
                let rect_w = rect_w.min(area.width);
                let rect_h = rect_h.min(area.height);
                let x = area.x + (area.width.saturating_sub(rect_w)) / 2;
                let y = area.y + (area.height.saturating_sub(rect_h)) / 2;
                let centered_rect = Rect::new(x, y, rect_w, rect_h);

                frame.render_widget(ViewportImageWidget(protocol), centered_rect);
            }
        }
        ImageDisplayState::Error(err) => {
            let err_block = Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(" Error Loading Image ")
                .style(Style::default().fg(Color::Red));
            let err_paragraph = Paragraph::new(err)
                .block(err_block)
                .alignment(Alignment::Center);
            frame.render_widget(err_paragraph, area);
        }
        ImageDisplayState::Empty => {
            if !model.is_loading {
                let loading_paragraph = Paragraph::new("No image loaded.")
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(Color::Yellow));
                frame.render_widget(loading_paragraph, area);
            }
        }
    }
}
