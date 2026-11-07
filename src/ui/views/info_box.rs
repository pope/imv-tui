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
    if area.width == 0 || area.height == 0 {
        return;
    }

    let title = " Image Details ";
    let w = 55.min(area.width.saturating_sub(1)).max(1);
    let h = app.palette_height.min(area.height.saturating_sub(1)).max(1);

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

        let x = x.min(area.right().saturating_sub(w));
        let y = y.min(area.bottom().saturating_sub(h));

        let popup_area = Rect::new(x, y, w, h);
        frame.render_widget(Clear, popup_area);
        frame.render_widget(palette_paragraph, popup_area);
    } else {
        let (filename, dir_str, disk_size_str, mem_size_str) =
            match &app.queue.images()[app.queue.current_index] {
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
            .images()
            .get(app.queue.current_index)
            .map(|src| src.is_raw())
            .unwrap_or(false);
        let dims = crate::imaging::types::PixelDimensions::new(app.img_width, app.img_height);
        let pixels_str = if dims.is_empty() {
            "0 x 0 px (0.0 MP)".to_string()
        } else {
            format!(
                "{} x {} px ({:.1} MP)",
                dims.width,
                dims.height,
                dims.megapixels()
            )
        };

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
                let raw_mp = (w as f64 * h as f64) / 1_000_000.0;
                format!("{} x {} px ({:.1} MP)", w, h, raw_mp)
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
            .queue
            .adjustment(app.queue.current_index)
            .rotation
            .to_degrees();
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
        let load_decode_val = match app.stats.load_duration {
            Some(dur) => format!("{:.2} ms", dur.as_secs_f64() * 1000.0).bold(),
            None => "Loading...".yellow(),
        };
        lines.push(Line::from(vec![
            "   Load / Decode: ".gray(),
            load_decode_val,
        ]));

        let thumb_load_line = if app.disable_thumbnail {
            Line::from(vec!["   Thumbnail Load: ".gray(), "N/A (Disabled)".gray()])
        } else if let Some(ref thumb) = app.stats.thumbnail {
            Line::from(vec![
                "   Thumbnail Load: ".gray(),
                format!(
                    "{:.2} ms @ {} x {} px ({})",
                    thumb.load_duration.as_secs_f64() * 1000.0,
                    thumb.dimensions.0,
                    thumb.dimensions.1,
                    thumb.source.display_label()
                )
                .bold(),
            ])
        } else {
            Line::from(vec!["   Thumbnail Load: ".gray(), "N/A".gray()])
        };
        lines.push(thumb_load_line);

        let (warm_count, warm_bytes) = app.prefetch_cache_stats();
        lines.push(Line::from(vec![
            "   Prefetch Cache: ".gray(),
            cache_hit_str,
            Span::raw(format!(
                " ({} warm, {})",
                warm_count,
                format_size(warm_bytes)
            ))
            .gray(),
        ]));
        lines.push(Line::from(vec![
            "   Uncompressed Mem: ".gray(),
            mem_size_str.as_str().bold(),
        ]));
        lines.push(Line::from(vec![
            "   Resize / Filter: ".gray(),
            format!(
                "{:.2} ms ({} @ {}%)",
                app.stats.process_duration.as_secs_f64() * 1000.0,
                app.filter_name(),
                app.current_zoom_pct.round() as i64
            )
            .bold(),
        ]));
        let proto_str = app.active_protocol.display_label();
        lines.push(Line::from(vec![
            "   Protocol Used: ".gray(),
            proto_str.bold(),
        ]));
        lines.push(Line::from(vec![
            "   Protocol Encode: ".gray(),
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
        let total_dur = app.stats.load_duration.unwrap_or_default()
            + app.stats.process_duration
            + app.stats.protocol_duration;
        lines.push(Line::from(vec![
            "   Total Pipeline: ".gray(),
            format!("{:.2} ms", total_dur.as_secs_f64() * 1000.0).bold(),
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

        let x = x.min(area.right().saturating_sub(w));
        let y = y.min(area.bottom().saturating_sub(h));

        let popup_area = Rect::new(x, y, w, h);
        frame.render_widget(Clear, popup_area);
        frame.render_widget(palette_paragraph, popup_area);
    }
}
