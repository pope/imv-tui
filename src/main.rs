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
use ratatui_image::picker::Picker;

use crate::app::{App, PaletteMode};
use crate::cli::{parse_cli_args, read_piped_stdin};
use crate::image_worker::{
    ImageSource, collect_sources, is_cbz_or_zip, list_cbz_pages, scan_directory,
};
use crate::ui::ui;

struct TerminalGuard;

impl TerminalGuard {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Check if we have piped input via stdin (e.g. from fd or find)
    let piped_files = read_piped_stdin();
    let is_piped = !piped_files.is_empty();

    // Parse CLI arguments
    let options = parse_cli_args()?;
    let initial_path = options.initial_path;
    let initial_filter = options.filter;
    let scale_mode = options.scale;
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

    // Query terminal protocol before raw mode
    let mut picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    if let Some(proto) = options.protocol {
        picker.set_protocol_type(proto);
    }

    // Setup terminal using RAII guard
    let _guard = TerminalGuard::new()?;
    let stdout = io::stdout();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app
    let mut app = App::new(images, current_index, picker, initial_filter, scale_mode)?;
    if let Some(cfg) = slideshow_opt {
        app.slideshow_config = cfg;
        app.slideshow_last_transition = std::time::Instant::now();
    }

    let mut last_loop_tick = std::time::Instant::now();

    // Main event loop
    while app.running {
        let now = std::time::Instant::now();
        let delta = now.duration_since(last_loop_tick);
        last_loop_tick = now;

        if app.slideshow_config.is_active() && app.palette_mode != PaletteMode::Closed {
            app.slideshow_last_transition += delta;
        }

        app.update_channels();

        // Automatic slideshow transition
        if app.slideshow_config.is_active()
            && !app.is_loading
            && app.palette_mode == PaletteMode::Closed
            && app.slideshow_last_transition.elapsed()
                >= std::time::Duration::from_secs(app.slideshow_config.seconds() as u64)
        {
            app.next_image();
            app.slideshow_last_transition = std::time::Instant::now();
        }

        let term_size = terminal.size().unwrap_or_default();
        app.update_layout(term_size.height);

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
            events
                .into_iter()
                .for_each(|ev| app.handle_event(ev, term_size.height));
        }
    }

    Ok(())
}

fn main() {
    // Register custom panic hook that restores standard terminal mode
    // before displaying panic details on console stderr.
    std::panic::set_hook(Box::new(|panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        eprintln!("Application Panicked:\n{}", panic_info);
    }));

    if let Err(e) = run() {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}
