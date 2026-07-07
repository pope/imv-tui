use crate::app::App;
use crate::config::InfoBarPosition;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

/// Formats raw byte counts to human-readable strings (KB, MB).
fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;

    if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Renders the technical details and EXIF / cache telemetry overlay box.
pub fn draw(frame: &mut Frame, app: &App, area: Rect) {
    let title = " Image Details ";
    let w = 55.min(area.width.saturating_sub(1));
    let h = app.palette_height.min(area.height.saturating_sub(1));

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

        let x = area.x + area.width.saturating_sub(w).saturating_sub(1);
        let y = if app.infobar == InfoBarPosition::Top {
            area.y
        } else {
            area.y.saturating_add(1)
        };

        let popup_area = Rect::new(x, y, w, h);
        frame.render_widget(Clear, popup_area);
        frame.render_widget(palette_paragraph, popup_area);
    } else {
        let (filename, dir_str, disk_size_str, mem_size_str) =
            match &app.queue.images[app.queue.current_index] {
                crate::imaging::ImageSource::Local(path) => {
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
                crate::imaging::ImageSource::Cbz {
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
        let is_raw = app
            .queue
            .images
            .get(app.queue.current_index)
            .map(|src| src.is_raw())
            .unwrap_or(false);
        let pixels_str = format!("{} x {} px", app.img_width, app.img_height);

        lines.push(Line::from(vec![
            " File: ".bold().cyan(),
            filename.as_str().into(),
        ]));
        lines.push(Line::from(vec![
            " Directory: ".bold().cyan(),
            dir_str.as_str().into(),
        ]));
        let mime_str = app
            .stats
            .format
            .map(|fmt| fmt.to_mime_type())
            .unwrap_or("image/unknown");
        let mime_label = if is_raw {
            " Preview MIME Type: "
        } else {
            " MIME Type: "
        };
        lines.push(Line::from(vec![mime_label.bold().cyan(), mime_str.into()]));
        lines.push(Line::from(vec![
            " Size on Disk: ".bold().cyan(),
            disk_size_str.as_str().into(),
        ]));
        let dims_label = if is_raw {
            " Preview Dimensions: "
        } else {
            " Dimensions: "
        };
        lines.push(Line::from(vec![
            dims_label.bold().cyan(),
            pixels_str.as_str().into(),
        ]));
        if is_raw {
            let raw_dims_str = if let Some(w) = app.stats.raw_width
                && let Some(h) = app.stats.raw_height
            {
                format!("{} x {} px", w, h)
            } else {
                "Unknown px".to_string()
            };
            lines.push(Line::from(vec![
                " RAW Dimensions: ".bold().cyan(),
                raw_dims_str.into(),
            ]));
        }
        let classification = app.current_classification();
        let flag_style = match classification {
            crate::app::Classification::Unflagged => Style::default().fg(Color::Gray),
            crate::app::Classification::Pick => Style::default().fg(Color::Green).bold(),
            crate::app::Classification::Reject => Style::default().fg(Color::Red).bold(),
        };
        let flag_label = classification.display_label();
        lines.push(Line::from(vec![
            " Flag State: ".bold().cyan(),
            Span::styled(flag_label, flag_style),
        ]));
        lines.push(Line::from(vec![
            " Brightness: ".bold().cyan(),
            format!("{:+}", app.current_brightness().value()).into(),
        ]));
        lines.push(Line::from(vec![
            " Contrast: ".bold().cyan(),
            format!("{:+}%", app.current_contrast().value().round() as i32).into(),
        ]));
        let rotation = app
            .adjustments
            .get(app.queue.current_index)
            .map(|adj| adj.rotation.to_degrees())
            .unwrap_or(0);
        lines.push(Line::from(vec![
            " Rotation: ".bold().cyan(),
            format!("{}°", rotation).into(),
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

        let thumb_load_line = if app.disable_thumbnail {
            Line::from(vec!["   Thumbnail Load: ".gray(), "N/A (Disabled)".gray()])
        } else {
            match (
                app.stats.thumbnail_load_duration,
                app.stats.thumbnail_dimensions,
            ) {
                (Some(dur), Some((tw, th))) => Line::from(vec![
                    "   Thumbnail Load: ".gray(),
                    format!("{:.2} ms @ {} x {} px", dur.as_secs_f64() * 1000.0, tw, th).bold(),
                ]),
                _ => Line::from(vec!["   Thumbnail Load: ".gray(), "N/A".gray()]),
            }
        };
        lines.push(thumb_load_line);

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
            "   Protocol Used: ".gray(),
            format!("{:?}", app.picker.protocol_type()).bold(),
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

        let x = area.x + area.width.saturating_sub(w).saturating_sub(1);
        let y = if app.infobar == InfoBarPosition::Top {
            area.y
        } else {
            area.y.saturating_add(1)
        };

        let popup_area = Rect::new(x, y, w, h);
        frame.render_widget(Clear, popup_area);
        frame.render_widget(palette_paragraph, popup_area);
    }
}
