mod app;
mod cli;
mod commands;
mod image_worker;
mod ui;

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use ratatui_image::picker::{Picker, ProtocolType};

use crate::app::App;
use crate::cli::{parse_cli_args, read_piped_stdin};
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

            let term_size = terminal.size().unwrap_or_default();
            for ev in events {
                app.handle_event(ev, term_size.height);
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
