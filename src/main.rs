mod app;
mod cli;
mod commands;
mod image_worker;
mod ui;

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use ratatui_image::picker::{Picker, ProtocolType};

use crate::app::{App, PaletteMode};
use crate::cli::{parse_cli_args, read_piped_stdin};
use crate::commands::Command;
use crate::image_worker::{
    FilterType, ImageSource, ScaleMode, collect_sources, is_cbz_or_zip, list_cbz_pages,
    scan_directory,
};
use crate::ui::ui;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Check if we have piped input via stdin (e.g. from fd or find)
    let piped_files = read_piped_stdin();
    let is_piped = !piped_files.is_empty();

    // Parse CLI arguments
    let options = match parse_cli_args() {
        Ok(opts) => opts,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    };
    let initial_path = options.initial_path;
    let filter_opt = options.filter;
    let protocol_opt = options.protocol;
    let scale_opt = options.scale;
    let slideshow_opt = options.slideshow;
    let check_magic = options.check_magic;

    // Get the image file list and current starting index
    let (images, current_index) = if is_piped {
        let sources = collect_sources(&piped_files, check_magic)?;
        (sources, 0)
    } else {
        let initial_path = initial_path.unwrap_or_else(|| PathBuf::from("."));
        if is_cbz_or_zip(&initial_path, check_magic) {
            let pages = list_cbz_pages(&initial_path)?;
            let sources = pages
                .into_iter()
                .map(|page| ImageSource::Cbz {
                    zip_path: initial_path.clone(),
                    file_in_zip: page,
                })
                .collect();
            (sources, 0)
        } else {
            let (paths, index) = scan_directory(&initial_path, check_magic)?;
            let sources = paths.into_iter().map(ImageSource::Local).collect();
            (sources, index)
        }
    };

    let initial_filter = match filter_opt.as_deref() {
        Some("nearest") => FilterType::Nearest,
        Some("linear") => FilterType::Triangle,
        Some("cubic") => FilterType::CatmullRom,
        Some("mitchell") => FilterType::Mitchell,
        Some("gaussian") => FilterType::Gaussian,
        Some("lanczos") => FilterType::Lanczos3,
        Some("hamming") => FilterType::Hamming,
        Some(other) => {
            eprintln!(
                "Error: Unknown filter '{}'. Choose from: nearest, linear, cubic, mitchell, gaussian, lanczos, hamming",
                other
            );
            std::process::exit(1);
        }
        None => FilterType::Nearest,
    };

    let scale_mode = match scale_opt.as_deref() {
        Some("none") | Some("actual") => ScaleMode::None,
        Some("shrink") => ScaleMode::Shrink,
        Some("full") | Some("fit") => ScaleMode::Full,
        Some("crop") => ScaleMode::Crop,
        Some(other) => {
            eprintln!(
                "Error: Unknown scale mode '{}'. Choose from: none, actual, shrink, full, crop",
                other
            );
            std::process::exit(1);
        }
        None => ScaleMode::Shrink,
    };

    // Query terminal protocol before raw mode
    let mut picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    if let Some(proto_str) = protocol_opt.as_deref() {
        let proto = match proto_str.to_lowercase().as_str() {
            "kitty" => ProtocolType::Kitty,
            "sixel" => ProtocolType::Sixel,
            "halfblocks" | "halfblock" => ProtocolType::Halfblocks,
            "iterm2" => ProtocolType::Iterm2,
            other => {
                eprintln!(
                    "Error: Unknown protocol '{}'. Choose from: kitty, sixel, halfblocks, iterm2",
                    other
                );
                std::process::exit(1);
            }
        };
        picker.set_protocol_type(proto);
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app
    let mut app = match App::new(images, current_index, picker, initial_filter, scale_mode) {
        Ok(app) => app,
        Err(e) => {
            // Restore terminal on init error
            disable_raw_mode()?;
            execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture)?;
            eprintln!("Initialization Error: {}", e);
            std::process::exit(1);
        }
    };
    if let Some(cfg) = slideshow_opt {
        app.slideshow_config = cfg;
        app.slideshow_last_transition = std::time::Instant::now();
    }

    // Main event loop
    while app.running {
        app.update_channels();

        // Automatic slideshow transition
        if app.slideshow_config.is_active()
            && !app.is_loading
            && app.slideshow_last_transition.elapsed()
                >= std::time::Duration::from_secs(app.slideshow_config.seconds() as u64)
        {
            app.next_image();
            app.slideshow_last_transition = std::time::Instant::now();
        }

        if app.needs_clear_once {
            app.needs_clear_once = false;
            terminal.clear()?;
        }

        if app.needs_clear {
            app.needs_clear = false;
            if app.should_clear_on_update() {
                terminal.clear()?;
            }
        }
        terminal.draw(|f| ui(f, &mut app))?;

        if event::poll(Duration::from_millis(50))? {
            let mut events = Vec::new();
            events.push(event::read()?);
            while event::poll(Duration::from_millis(0))? {
                events.push(event::read()?);
            }

            for ev in events {
                match ev {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        if app.palette_mode != PaletteMode::Closed {
                            match key.code {
                                KeyCode::Esc => {
                                    app.palette_mode = PaletteMode::Closed;
                                    app.needs_update = true;
                                    app.needs_clear_once = true;
                                }
                                KeyCode::Enter => match app.palette_mode {
                                    PaletteMode::File => {
                                        let files = app.get_filtered_files();
                                        if !files.is_empty()
                                            && app.palette_selected_index < files.len()
                                        {
                                            app.queue.current_index = files[app.palette_selected_index].0;
                                            app.start_load_image();
                                        }
                                        app.palette_mode = PaletteMode::Closed;
                                        app.needs_update = true;
                                        app.needs_clear_once = true;
                                    }
                                    PaletteMode::Command => {
                                        let cmds = app.get_filtered_commands();
                                        if !cmds.is_empty()
                                            && app.palette_selected_index < cmds.len()
                                        {
                                            let cmd = cmds[app.palette_selected_index].cmd;
                                            app.execute_command(cmd);
                                        }
                                        if app.palette_mode == PaletteMode::Command {
                                            app.palette_mode = PaletteMode::Closed;
                                            app.needs_update = true;
                                            app.needs_clear_once = true;
                                        }
                                    }
                                    PaletteMode::Prompt => {
                                        if let Some(prompt_type) = app.prompt_type {
                                            app.execute_prompt(prompt_type);
                                        }
                                    }
                                    _ => {}
                                },
                                KeyCode::Up if app.palette_selected_index > 0 => {
                                    app.palette_selected_index -= 1;
                                }
                                KeyCode::Down => {
                                    let max_len = match app.palette_mode {
                                        PaletteMode::File => app.get_filtered_files().len(),
                                        PaletteMode::Command => app.get_filtered_commands().len(),
                                        _ => 0,
                                    };
                                    if max_len > 0 && app.palette_selected_index < max_len - 1 {
                                        app.palette_selected_index += 1;
                                    }
                                }
                                KeyCode::PageUp => {
                                    let max_len = match app.palette_mode {
                                        PaletteMode::File => app.get_filtered_files().len(),
                                        PaletteMode::Command => app.get_filtered_commands().len(),
                                        _ => 0,
                                    };
                                    let term_size = terminal.size().unwrap_or_default();
                                    let viewport_h = term_size.height.saturating_sub(1);
                                    let max_h = (viewport_h as f64 * 0.5).round() as u16;
                                    let palette_h = (max_len as u16 + 4).max(12).min(max_h);
                                    let page_size = (palette_h as usize).saturating_sub(4);

                                    app.palette_selected_index =
                                        app.palette_selected_index.saturating_sub(page_size);
                                }
                                KeyCode::PageDown => {
                                    let max_len = match app.palette_mode {
                                        PaletteMode::File => app.get_filtered_files().len(),
                                        PaletteMode::Command => app.get_filtered_commands().len(),
                                        _ => 0,
                                    };
                                    if max_len > 0 {
                                        let term_size = terminal.size().unwrap_or_default();
                                        let viewport_h = term_size.height.saturating_sub(1);
                                        let max_h = (viewport_h as f64 * 0.5).round() as u16;
                                        let palette_h = (max_len as u16 + 4).max(12).min(max_h);
                                        let page_size = (palette_h as usize).saturating_sub(4);

                                        app.palette_selected_index = (app.palette_selected_index
                                            + page_size)
                                            .min(max_len - 1);
                                    }
                                }
                                KeyCode::Char('k')
                                    if key.modifiers.contains(event::KeyModifiers::CONTROL)
                                        && app.palette_selected_index > 0 =>
                                {
                                    app.palette_selected_index -= 1;
                                }
                                KeyCode::Char('j')
                                    if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                                {
                                    let max_len = match app.palette_mode {
                                        PaletteMode::File => app.get_filtered_files().len(),
                                        PaletteMode::Command => app.get_filtered_commands().len(),
                                        _ => 0,
                                    };
                                    if max_len > 0 && app.palette_selected_index < max_len - 1 {
                                        app.palette_selected_index += 1;
                                    }
                                }
                                KeyCode::Backspace => {
                                    app.palette_pop_char();
                                }
                                KeyCode::Char(c) => {
                                    app.palette_push_char(c);
                                }
                                _ => {}
                            }
                        } else {
                            if let Some(cmd) = Command::from_key(key) {
                                app.execute_command(cmd);
                            }
                        }
                    }
                    Event::Mouse(mouse_event) => match mouse_event.kind {
                        MouseEventKind::ScrollUp => {
                            app.execute_command(Command::ZoomIn);
                        }
                        MouseEventKind::ScrollDown => {
                            app.execute_command(Command::ZoomOut);
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
    }

    // Cleanup terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}
