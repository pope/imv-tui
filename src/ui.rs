use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Style, Stylize},
    text::Line,
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use ratatui_image::StatefulImage;
use std::time::Duration;

use crate::app::{App, PaletteMode, PromptType};

/// Renders the entire view layout: including centered images (via Kitty, Sixel,
/// or Halfblocks protocol), error details, loading spinners, bottom status HUD,
/// and interactive command/file palette overlays.
///
/// NOTE: To preserve the separation of concerns, the rendering function must remain
/// strictly side-effect free and read-only. No application state adjustments or selection
/// boundary clamping are performed inside this function.
pub fn ui(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(frame.area());

    // Update protocol if size has changed or update is requested
    let widget_size = (chunks[0].width, chunks[0].height);
    if app.needs_update || app.last_widget_size != widget_size {
        app.last_widget_size = widget_size;
        app.needs_update = false;
        app.update_protocol(widget_size.0, widget_size.1);
    }

    // Render image or placeholders
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
        frame.render_widget(loading_paragraph, chunks[0]);
    } else if let Some(ref mut protocol) = app.image_protocol {
        // Calculate the centered Rect inside chunks[0]
        let (rect_w, rect_h) = app.rendered_size_cells;
        let rect_w = rect_w.min(chunks[0].width);
        let rect_h = rect_h.min(chunks[0].height);
        let x = chunks[0].x + (chunks[0].width.saturating_sub(rect_w)) / 2;
        let y = chunks[0].y + (chunks[0].height.saturating_sub(rect_h)) / 2;
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
        frame.render_widget(err_paragraph, chunks[0]);
    } else {
        let loading_paragraph = Paragraph::new("No image loaded.")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::Yellow));
        frame.render_widget(loading_paragraph, chunks[0]);
    }

    if app.queue.is_empty() {
        let status_block = Block::default()
            .title(" imv-tui ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Cyan))
            .title_style(Style::default().fg(Color::Yellow).bold());
        let inner_rect = status_block.inner(chunks[1]);
        frame.render_widget(status_block, chunks[1]);
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
        if app.slideshow_config.is_active() {
            extra_info.push_str(&format!(
                " | Slideshow: {}s",
                app.slideshow_config.seconds()
            ));
        }

        let title_text = format!(" {} {} ", app.current_icon, app.current_filename());
        let status_block = Block::default()
            .title(title_text)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Cyan))
            .title_style(Style::default().fg(Color::Yellow).bold());
        let inner_rect = status_block.inner(chunks[1]);
        frame.render_widget(status_block, chunks[1]);

        let status_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(30),
                Constraint::Min(0),
                Constraint::Length(22),
            ])
            .split(inner_rect);

        let left_text = format!(
            " [{}/{}] ({}x{}){} ",
            app.queue.current_index + 1,
            app.queue.images.len(),
            app.img_width,
            app.img_height,
            if app.show_thumbnail_only {
                " [THUMB]"
            } else {
                ""
            }
        );
        let left_para = Paragraph::new(left_text)
            .alignment(Alignment::Left)
            .style(Style::default().fg(Color::White).bg(Color::Reset));
        frame.render_widget(left_para, status_chunks[0]);

        let mid_text = format!(
            "Scale: {} | Filter: {} | Zoom: {}% | Pan: ({}, {}){}",
            app.scale_mode.name(),
            app.filter_name(),
            app.current_zoom_pct.round() as i64,
            app.pan_offset.x,
            app.pan_offset.y,
            extra_info
        );
        let mid_para = Paragraph::new(mid_text)
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::White).bg(Color::Reset));
        frame.render_widget(mid_para, status_chunks[1]);

        let right_text = "Press '?' for commands ";
        let right_para = Paragraph::new(right_text)
            .alignment(Alignment::Right)
            .style(Style::default().fg(Color::White).bg(Color::Reset));
        frame.render_widget(right_para, status_chunks[2]);
    }

    // Command / File Palette popup
    if app.palette_mode != PaletteMode::Closed {
        if app.palette_mode == PaletteMode::Prompt {
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
                Some(PromptType::SetSlideshow) => {
                    "Enter slideshow delay in seconds (e.g. 5, +1, -1):"
                }
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

            let w = 45.min(chunks[0].width.saturating_sub(1));
            let h = app.palette_height.min(chunks[0].height.saturating_sub(1));

            let palette_block = Block::default()
                .title(prompt_title)
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Cyan))
                .title_style(Style::default().fg(Color::Yellow).bold());

            let palette_paragraph = Paragraph::new(lines)
                .block(palette_block)
                .style(Style::default().fg(Color::White).bg(Color::Reset));

            let x = chunks[0].x + chunks[0].width.saturating_sub(w).saturating_sub(1);
            let y = chunks[0].y.saturating_add(1);

            let popup_area = Rect::new(x, y, w, h);
            frame.render_widget(Clear, popup_area);
            frame.render_widget(palette_paragraph, popup_area);
        } else if app.palette_mode == PaletteMode::Info {
            let title = " Image Details ";
            let w = 55.min(chunks[0].width.saturating_sub(1));
            let h = app.palette_height.min(chunks[0].height.saturating_sub(1));

            let mut lines = Vec::new();

            if app.queue.is_empty() {
                lines.push(Line::from(" No image details available.".gray().italic()));

                let palette_block = Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title_style(Style::default().fg(Color::Yellow).bold());

                let palette_paragraph = Paragraph::new(lines)
                    .block(palette_block)
                    .style(Style::default().fg(Color::White).bg(Color::Reset));

                let x = chunks[0].x + chunks[0].width.saturating_sub(w).saturating_sub(1);
                let y = chunks[0].y.saturating_add(1);

                let popup_area = Rect::new(x, y, w, h);
                frame.render_widget(Clear, popup_area);
                frame.render_widget(palette_paragraph, popup_area);
            } else {
                let (filename, dir_str, disk_size_str, mem_size_str) =
                    match &app.queue.images[app.queue.current_index] {
                        crate::image_worker::ImageSource::Local(path) => {
                            let filename = path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("Unknown")
                                .to_string();
                            let dir_str = path
                                .parent()
                                .and_then(|p| p.to_str())
                                .unwrap_or("Unknown")
                                .to_string();
                            let disk_size_str = format_size(app.stats.disk_size);
                            let mem_size = app
                                .original_image
                                .as_ref()
                                .map(|img| img.as_bytes().len() as u64)
                                .unwrap_or(0);
                            let mem_size_str = format_size(mem_size);
                            (filename, dir_str, disk_size_str, mem_size_str)
                        }
                        crate::image_worker::ImageSource::Cbz {
                            zip_path,
                            file_in_zip,
                        } => {
                            let filename = file_in_zip.clone();
                            let dir_str = format!("{} (Archive)", zip_path.display());
                            let disk_size_str = format_size(app.stats.disk_size);
                            let mem_size = app
                                .original_image
                                .as_ref()
                                .map(|img| img.as_bytes().len() as u64)
                                .unwrap_or(0);
                            let mem_size_str = format_size(mem_size);
                            (filename, dir_str, disk_size_str, mem_size_str)
                        }
                    };
                let pixels_str = format!("{} x {} px", app.img_width, app.img_height);

                lines.push(Line::from(vec![
                    " File: ".bold().cyan(),
                    filename.as_str().into(),
                ]));
                lines.push(Line::from(vec![
                    " Directory: ".bold().cyan(),
                    dir_str.as_str().into(),
                ]));
                lines.push(Line::from(vec![
                    " Size on Disk: ".bold().cyan(),
                    disk_size_str.as_str().into(),
                ]));
                lines.push(Line::from(vec![
                    " Dimensions: ".bold().cyan(),
                    pixels_str.as_str().into(),
                ]));

                let inner_w = w.saturating_sub(2) as usize;
                lines.push(Line::from("─".repeat(inner_w).gray()));
                lines.push(Line::from(" Stats for Nerds:".bold().yellow()));

                let cache_hit_str = if app.stats.is_prefetch_cache_hit {
                    "Yes (Hit)".green()
                } else {
                    "No (Miss)".red()
                };
                lines.push(Line::from(vec![
                    "   Load / Decode: ".gray(),
                    format!("{:.2} ms", app.stats.load_duration.as_secs_f64() * 1000.0).bold(),
                ]));
                let thumb_load_str = if app.disable_thumbnail {
                    "N/A (Disabled)".gray()
                } else {
                    match app.stats.thumbnail_load_duration {
                        Some(dur) => format!("{:.2} ms", dur.as_secs_f64() * 1000.0).bold(),
                        None => "N/A (No EXIF Thumbnail / Large Image)".gray(),
                    }
                };
                lines.push(Line::from(vec![
                    "   Thumbnail Load: ".gray(),
                    thumb_load_str,
                ]));
                lines.push(Line::from(vec![
                    "   Thumbnail Mode: ".gray(),
                    if app.show_thumbnail_only {
                        "Active (Thumbnail Displayed)".green().bold()
                    } else if app.thumbnail_image.is_some() {
                        "Inactive (Full Image Displayed)".yellow()
                    } else {
                        "N/A".gray()
                    },
                ]));
                let thumb_dim_str = match app.stats.thumbnail_dimensions {
                    Some((w, h)) => format!("{} x {} px", w, h).bold(),
                    None => "N/A".gray(),
                };
                lines.push(Line::from(vec![
                    "   Thumbnail Dimensions: ".gray(),
                    thumb_dim_str,
                ]));
                lines.push(Line::from(vec![
                    "   Prefetch Cache Hit: ".gray(),
                    cache_hit_str,
                ]));
                lines.push(Line::from(vec![
                    "   Uncompressed Mem: ".gray(),
                    mem_size_str.as_str().bold(),
                ]));
                lines.push(Line::from(vec![
                    "   Resize / Filter: ".gray(),
                    format!(
                        "{:.2} ms",
                        app.stats.process_duration.as_secs_f64() * 1000.0
                    )
                    .bold(),
                ]));
                lines.push(Line::from(vec![
                    "   Terminal API Write: ".gray(),
                    format!(
                        "{:.2} ms",
                        app.stats.protocol_duration.as_secs_f64() * 1000.0
                    )
                    .bold(),
                ]));
                let proto_pixels_str = format!(
                    "{} x {} px",
                    app.stats.protocol_width, app.stats.protocol_height
                );
                lines.push(Line::from(vec![
                    "   Protocol Pixels: ".gray(),
                    proto_pixels_str.as_str().bold(),
                ]));

                let palette_block = Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title_style(Style::default().fg(Color::Yellow).bold());

                let palette_paragraph = Paragraph::new(lines)
                    .block(palette_block)
                    .style(Style::default().fg(Color::White).bg(Color::Reset));

                let x = chunks[0].x + chunks[0].width.saturating_sub(w).saturating_sub(1);
                let y = chunks[0].y.saturating_add(1);

                let popup_area = Rect::new(x, y, w, h);
                frame.render_widget(Clear, popup_area);
                frame.render_widget(palette_paragraph, popup_area);
            }
        } else {
            let title = match app.palette_mode {
                PaletteMode::File => " File Search ",
                PaletteMode::Command => " Command Palette ",
                _ => "",
            };

            let visible_count = (app.palette_height as usize).saturating_sub(4);
            let palette_height = app.palette_height;

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
                    let start_idx = if total_files <= visible_count
                        || app.palette_selected_index < half_visible
                    {
                        0
                    } else if app.palette_selected_index >= total_files.saturating_sub(half_visible)
                    {
                        total_files.saturating_sub(visible_count)
                    } else {
                        app.palette_selected_index.saturating_sub(half_visible)
                    };

                    for (i, (_, filename)) in filtered_files
                        .iter()
                        .enumerate()
                        .skip(start_idx)
                        .take(visible_count)
                    {
                        let line = if i == app.palette_selected_index {
                            Line::from(vec![
                                " > ".bold().yellow().on_blue(),
                                filename.as_str().bold().yellow().on_blue(),
                            ])
                        } else {
                            Line::from(vec!["   ".into(), filename.as_str().into()])
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
                    let start_idx = if total_cmds <= visible_count
                        || app.palette_selected_index < half_visible
                    {
                        0
                    } else if app.palette_selected_index >= total_cmds.saturating_sub(half_visible)
                    {
                        total_cmds.saturating_sub(visible_count)
                    } else {
                        app.palette_selected_index.saturating_sub(half_visible)
                    };

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
            let cap_width = (chunks[0].width as f64 * 0.75).round() as u16;
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

            let w = palette_width.min(chunks[0].width.saturating_sub(1));
            let h = palette_height.min(chunks[0].height.saturating_sub(1));
            let x = chunks[0].x + chunks[0].width.saturating_sub(w).saturating_sub(1);
            let y = chunks[0].y.saturating_add(1);

            let popup_area = Rect::new(x, y, w, h);

            frame.render_widget(Clear, popup_area);
            frame.render_widget(palette_paragraph, popup_area);
        }
    }
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.2} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}
