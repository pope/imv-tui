mod app;
mod commands;
mod config;
mod imaging;
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

use crate::app::{App, Classification, PaletteMode};
use crate::config::cli::{parse_cli_args, read_piped_stdin};
use crate::imaging::{ImageSource, collect_sources, is_cbz_or_zip, list_cbz_pages, scan_directory};
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
    let mut app = App::new(
        images,
        current_index,
        picker,
        initial_filter,
        scale_mode,
        options.no_thumbnail,
        options.infobar,
    )?;
    if let Some(state) = slideshow_opt {
        app.slideshow_state = state;
        app.slideshow_last_transition = std::time::Instant::now();
    }
    if let Some(ref path) = options.import_path {
        app.import_classifications(path).map_err(io::Error::other)?;
    }

    let mut draw_needed = true;
    let mut last_loop_tick = std::time::Instant::now();

    // Main event loop
    while app.running {
        let now = std::time::Instant::now();
        let delta = now.duration_since(last_loop_tick);
        last_loop_tick = now;

        if app.slideshow_state.is_active()
            && (app.palette_mode != PaletteMode::Closed || app.slideshow_state.is_paused())
        {
            app.slideshow_last_transition += delta;
        }

        if app.update_channels() {
            draw_needed = true;
        }

        // Automatic slideshow transition
        if app.slideshow_state.is_playing()
            && !app.is_loading
            && app.palette_mode == PaletteMode::Closed
            && app.slideshow_last_transition.elapsed()
                >= std::time::Duration::from_secs(app.slideshow_state.seconds() as u64)
        {
            app.next_image();
            app.slideshow_last_transition = std::time::Instant::now();
            draw_needed = true;
        }

        let term_size = terminal.size().unwrap_or_default();
        let widget_w = term_size.width;
        let widget_h = term_size.height.saturating_sub(3);
        if app.last_widget_size != (widget_w, widget_h) {
            draw_needed = true;
        }

        app.update_layout(term_size.width, term_size.height);

        if app.needs_clear_once {
            app.needs_clear_once = false;
            terminal.clear()?;
            draw_needed = true;
        }

        if app.needs_clear {
            app.needs_clear = false;
            if app.should_clear_on_update() {
                terminal.clear()?;
            }
            draw_needed = true;
        }

        if draw_needed {
            draw_needed = false;
            terminal.draw(|f| ui(f, &mut app))?;
        }

        if event::poll(Duration::from_millis(33))? {
            let mut events = Vec::new();
            events.push(event::read()?);
            while event::poll(Duration::from_millis(0))? {
                events.push(event::read()?);
            }

            let term_size = terminal.size().unwrap_or_default();
            // Only redraw on actual user inputs or window changes. Non-user events (like Kitty/Sixel
            // graphics query response escape sequences written to stdin) are processed but ignored
            // for redrawing to prevent infinite drawing feedback loops and high CPU usage.
            let mut meaningful = false;
            for ev in events {
                meaningful |= matches!(
                    ev,
                    event::Event::Key(_)
                        | event::Event::Resize(_, _)
                        | event::Event::Mouse(_)
                        | event::Event::Paste(_)
                );
                app.handle_event(ev, term_size.height);
            }
            if meaningful {
                draw_needed = true;
            }
        }
    }

    drop(_guard);

    if let Some(ref path) = options.export_path {
        app.export_classifications(path).map_err(io::Error::other)?;
    } else {
        // Output flagged files to stdout using tab-separated format
        for (idx, img) in app.queue.images.iter().enumerate() {
            let class = app
                .classifications
                .get(idx)
                .cloned()
                .unwrap_or(Classification::Unflagged);
            if class != Classification::Unflagged {
                let state_str = match class {
                    Classification::Pick => "PICK",
                    Classification::Reject => "REJECT",
                    Classification::Unflagged => "UNFLAGGED",
                };
                println!("{}\t{}", state_str, img.identifier());
            }
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
