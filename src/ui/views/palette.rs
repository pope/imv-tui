use crate::app::{App, PaletteMode};
use crate::config::InfoBarPosition;
use ratatui::{
    Frame,
    layout::{Margin, Rect},
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, Clear, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState,
    },
};

/// Renders the searchable command palette or file search input overlays.
pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let title = match app.palette_mode {
        PaletteMode::File => " File Search ",
        PaletteMode::Command => " Command Palette ",
        _ => "",
    };

    let mut palette_width = app.palette_width;
    let cap_width = (area.width as f64 * 0.75).round() as u16;
    palette_width = palette_width.max(40).min(cap_width);
    let inner_w = palette_width.saturating_sub(2) as usize;

    let visible_count = (app.palette_height as usize).saturating_sub(4);
    let palette_height = app.palette_height;
    let mut scroll_pos = 0;
    let mut total_items = 0;

    let mut lines = vec![
        Line::from(vec![" > ".bold().cyan(), app.palette_query.value().into()]),
        Line::from("─".repeat(inner_w).gray()),
    ];

    match app.palette_mode {
        PaletteMode::File => {
            let filtered_files = app.get_filtered_files();

            let total_files = filtered_files.len();
            let half_visible = visible_count / 2;
            let start_idx =
                if total_files <= visible_count || app.palette_selected_index < half_visible {
                    0
                } else if app.palette_selected_index >= total_files.saturating_sub(half_visible) {
                    total_files.saturating_sub(visible_count)
                } else {
                    app.palette_selected_index.saturating_sub(half_visible)
                };
            scroll_pos = start_idx;
            total_items = total_files;

            let max_path_len = inner_w.saturating_sub(6);

            for (i, item) in filtered_files
                .iter()
                .enumerate()
                .skip(start_idx)
                .take(visible_count)
            {
                let orig_idx = item.0;
                let filename = &item.1;
                let display_path = shorten_path(filename, max_path_len);
                let class = app
                    .classifications
                    .get(orig_idx)
                    .cloned()
                    .unwrap_or(crate::app::Classification::Unflagged);
                let class_prefix = class.search_prefix();
                let line = if i == app.palette_selected_index {
                    Line::from(vec![
                        " > ".bold().yellow().on_blue(),
                        class_prefix.bold().yellow().on_blue(),
                        display_path.bold().yellow().on_blue(),
                    ])
                } else {
                    Line::from(vec!["   ".into(), class_prefix.into(), display_path.into()])
                };
                lines.push(line);
            }

            if filtered_files.is_empty() {
                lines.push(Line::from("   No matches found.".gray().italic()));
            }
        }
        PaletteMode::Command => {
            let filtered_commands = app.get_filtered_commands();

            let total_cmds = filtered_commands.len();
            let half_visible = visible_count / 2;
            let start_idx =
                if total_cmds <= visible_count || app.palette_selected_index < half_visible {
                    0
                } else if app.palette_selected_index >= total_cmds.saturating_sub(half_visible) {
                    total_cmds.saturating_sub(visible_count)
                } else {
                    app.palette_selected_index.saturating_sub(half_visible)
                };
            scroll_pos = start_idx;
            total_items = total_cmds;

            for (i, cmd) in filtered_commands
                .iter()
                .enumerate()
                .skip(start_idx)
                .take(visible_count)
            {
                let mut cmd_line = vec![
                    if i == app.palette_selected_index {
                        " > "
                    } else {
                        "   "
                    }
                    .into(),
                    cmd.item.name.bold(),
                ];

                if !cmd.shortcut_str.is_empty() {
                    cmd_line.push(" [".into());
                    cmd_line.push(cmd.shortcut_str.as_str().cyan());
                    cmd_line.push("]".into());
                }

                cmd_line.push(" - ".into());
                cmd_line.push(cmd.item.description.gray());
                let mut line = Line::from(cmd_line);
                if i == app.palette_selected_index {
                    line = line.yellow().on_blue();
                }
                lines.push(line);
            }

            if filtered_commands.is_empty() {
                lines.push(Line::from("   No matches found.".gray().italic()));
            }
        }
        _ => {}
    }

    let palette_block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .title_style(Style::default().fg(Color::Yellow).bold());

    let palette_paragraph = Paragraph::new(lines)
        .block(palette_block)
        .style(Style::default().fg(Color::White).bg(Color::Reset));

    let w = palette_width.min(area.width.saturating_sub(1));
    let h = palette_height.min(area.height.saturating_sub(1));
    let x = area.x + area.width.saturating_sub(w).saturating_sub(1);
    let y = if app.infobar == InfoBarPosition::Top {
        area.y
    } else {
        area.y.saturating_add(1)
    };

    let popup_area = Rect::new(x, y, w, h);

    frame.render_widget(Clear, popup_area);
    frame.render_widget(palette_paragraph, popup_area);

    if total_items > visible_count {
        let mut scrollbar_state =
            ScrollbarState::new(total_items.saturating_sub(visible_count) + 1).position(scroll_pos);

        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"))
            .track_symbol(Some("│"))
            .thumb_symbol("║")
            .thumb_style(Style::default().bold());

        frame.render_stateful_widget(
            scrollbar,
            popup_area.inner(Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut scrollbar_state,
        );
    }

    let cursor_byte_offset = app.palette_query.cursor_byte_offset();
    let prefix = &app.palette_query.value()[..cursor_byte_offset];
    let cursor_col = Span::raw(prefix).width() as u16;

    frame.set_cursor_position((popup_area.x + 4 + cursor_col, popup_area.y + 1));
}

fn shorten_path(path_str: &str, max_len: usize) -> String {
    if path_str.len() <= max_len {
        return path_str.to_string();
    }

    let path = std::path::Path::new(path_str);
    let components: Vec<&str> = path
        .components()
        .map(|c| c.as_os_str().to_str().unwrap_or(""))
        .filter(|s| !s.is_empty())
        .collect();

    if components.is_empty() {
        return path_str.to_string();
    }

    let last = components[components.len() - 1];

    let get_elided_suffix = |s: &str, elided_len: usize| -> String {
        let char_indices: Vec<(usize, char)> = s.char_indices().collect();
        if char_indices.len() > elided_len {
            let start_idx = char_indices[char_indices.len() - elided_len].0;
            format!("...{}", &s[start_idx..])
        } else {
            s.to_string()
        }
    };

    if components.len() <= 2 {
        if last.len() <= max_len {
            last.to_string()
        } else {
            get_elided_suffix(last, max_len.saturating_sub(3))
        }
    } else {
        let first = components[0];
        let collapsed = format!("{}/.../{}", first, last);
        if collapsed.len() <= max_len {
            collapsed
        } else if last.len() <= max_len {
            last.to_string()
        } else {
            get_elided_suffix(last, max_len.saturating_sub(3))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shorten_path() {
        // Fits under max_len as-is
        assert_eq!(shorten_path("a/b/c/d/e.png", 20), "a/b/c/d/e.png");

        // Exceeds max_len, collapses intermediate directories
        assert_eq!(shorten_path("a/b/c/d/e.png", 12), "a/.../e.png");

        // Collapsed path still exceeds max_len, returns filename
        assert_eq!(shorten_path("verylongname/nested/lake.png", 20), "lake.png");

        // Collapsed path fits max_len
        assert_eq!(
            shorten_path("verylongname/nested/lake.png", 26),
            "verylongname/.../lake.png"
        );

        // Fits under max_len as-is
        assert_eq!(shorten_path("short/lake.png", 20), "short/lake.png");

        // Filename itself exceeds max_len, truncates filename with leading ...
        assert_eq!(
            shorten_path("a/very_long_nested_filename_that_exceeds_max.png", 20),
            "...t_exceeds_max.png"
        );
    }
}
