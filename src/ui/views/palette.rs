use crate::app::{App, PaletteMode};
use crate::config::InfoBarPosition;
use ratatui::{
    Frame,
    layout::{Margin, Rect},
    style::{Color, Style, Stylize},
    text::Line,
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

    let visible_count = (app.palette_height as usize).saturating_sub(4);
    let palette_height = app.palette_height;
    let mut scroll_pos = 0;
    let mut total_items = 0;

    let mut lines = vec![
        Line::from(vec![
            " > ".bold().cyan(),
            app.palette_query.as_str().into(),
            "▊".cyan(), // cursor block
        ]),
        Line::from("──────────────────────────────────────────────────────────".gray()),
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

            for (i, item) in filtered_files
                .iter()
                .enumerate()
                .skip(start_idx)
                .take(visible_count)
            {
                let orig_idx = item.0;
                let filename = &item.1;
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
                        filename.as_str().bold().yellow().on_blue(),
                    ])
                } else {
                    Line::from(vec![
                        "   ".into(),
                        class_prefix.into(),
                        filename.as_str().into(),
                    ])
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

    let mut palette_width = app.palette_width;
    let cap_width = (area.width as f64 * 0.75).round() as u16;
    palette_width = palette_width.max(40).min(cap_width);

    if lines.len() > 1 {
        let inner_w = palette_width.saturating_sub(2) as usize;
        lines[1] = Line::from("─".repeat(inner_w).gray());
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
}
