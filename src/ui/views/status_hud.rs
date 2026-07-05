use crate::app::{App, Classification};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, BorderType, Borders, Paragraph},
};

/// Renders the bottom HUD status bar, file index/mode details, and active filter configuration.
pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    if app.queue.is_empty() {
        let status_block = Block::default()
            .title(" imv-tui ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Cyan))
            .title_style(Style::default().fg(Color::Yellow).bold());
        let inner_rect = status_block.inner(area);
        frame.render_widget(status_block, area);
        let empty_para = Paragraph::new(" No files found. Press 'q' to quit. ")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::White).bg(Color::Reset));
        frame.render_widget(empty_para, inner_rect);
    } else {
        let mut extra_info = String::new();
        if app.brightness.value() != 0 {
            extra_info.push_str(&format!(" | Brightness: {:+}", app.brightness.value()));
        }
        if app.contrast.value() != 0.0 {
            extra_info.push_str(&format!(
                " | Contrast: {:+}%",
                app.contrast.value().round() as i32
            ));
        }
        if app.slideshow_state.is_active() {
            if app.slideshow_state.is_paused() {
                extra_info.push_str(&format!(
                    " | Slideshow: Paused ({}s)",
                    app.slideshow_state.seconds()
                ));
            } else {
                extra_info.push_str(&format!(" | Slideshow: {}s", app.slideshow_state.seconds()));
            }
        }

        let classification = app.current_classification();
        let flag_icon = classification.icon();

        let filename_color = match classification {
            Classification::Pick => Color::Green,
            Classification::Reject => Color::Red,
            Classification::Unflagged => Color::Yellow,
        };

        let left_title_line = ratatui::text::Line::from(vec![
            ratatui::text::Span::raw(format!(" {} ", flag_icon)),
            ratatui::text::Span::styled(
                app.current_filename(),
                Style::default().fg(filename_color).bold(),
            ),
            ratatui::text::Span::raw(" "),
        ])
        .left_aligned();

        let right_title_line =
            ratatui::text::Line::from(format!(" [{}] ", app.view_mode_name())).right_aligned();

        let status_block = Block::default()
            .title(left_title_line)
            .title(right_title_line)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Cyan))
            .title_style(Style::default().fg(Color::Yellow).bold());
        let inner_rect = status_block.inner(area);
        frame.render_widget(status_block, area);

        let left_text = format!(
            " [{}/{}] ({}x{}){} ",
            app.get_visible_position().map(|pos| pos + 1).unwrap_or(0),
            app.get_visible_count(),
            app.img_width,
            app.img_height,
            if app.show_thumbnail_only {
                " [THUMB]"
            } else {
                ""
            }
        );

        let mid_text = format!(
            "Scale: {} | Filter: {} | Zoom: {}% | Pan: ({}, {}){}",
            app.scale_mode.name(),
            app.filter_name(),
            app.current_zoom_pct.round() as i64,
            app.pan_offset.x,
            app.pan_offset.y,
            extra_info
        );

        let right_text = "Press '?' for commands ";

        let left_len = left_text.chars().count() as u16;
        let right_len = right_text.chars().count() as u16;
        let side_len = left_len.max(right_len);

        let status_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(side_len),
                Constraint::Min(0),
                Constraint::Length(side_len),
            ])
            .split(inner_rect);

        let left_para = Paragraph::new(left_text)
            .alignment(Alignment::Left)
            .style(Style::default().fg(Color::White).bg(Color::Reset));
        frame.render_widget(left_para, status_chunks[0]);

        let mid_para = Paragraph::new(mid_text)
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::White).bg(Color::Reset));
        frame.render_widget(mid_para, status_chunks[1]);

        let right_para = Paragraph::new(right_text)
            .alignment(Alignment::Right)
            .style(Style::default().fg(Color::White).bg(Color::Reset));
        frame.render_widget(right_para, status_chunks[2]);
    }
}
