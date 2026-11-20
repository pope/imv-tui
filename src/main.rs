//! Fast, keyboard-driven terminal image viewer (imv-tui).

#![warn(missing_docs)]

mod app;
mod commands;
mod config;
mod imaging;
mod ui;

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use crate::app::{App, ImageDisplayState};
use crate::config::cli::{parse_cli_args, read_piped_stdin, reopen_tty_stdin};
use crate::imaging::{
    ImageSource, Picker, QueryStdioOptions, collect_sources, is_cbz_or_zip, list_cbz_pages,
    scan_directory,
};
use crate::ui::ui;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture},
    execute, queue,
    terminal::{
        BeginSynchronizedUpdate, EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen,
        disable_raw_mode, enable_raw_mode,
    },
};
use ratatui::{
    Terminal,
    backend::{Backend, ClearType, CrosstermBackend},
};

struct TerminalGuard;

impl TerminalGuard {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        enable_raw_mode()?;
        let guard = Self;
        execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        crate::imaging::cleanup_all_kitty_shm_segments();
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            EndSynchronizedUpdate,
            LeaveAlternateScreen,
            DisableMouseCapture
        );
    }
}

/// RAII guard for DEC Mode 2026 Synchronized Output (`?2026h` / `?2026l`).
///
/// Ensures `EndSynchronizedUpdate` is always emitted and flushed even if
/// clearing or drawing fails partway through a frame.
struct SyncUpdateGuard<'a, W: io::Write> {
    terminal: &'a mut Terminal<CrosstermBackend<W>>,
    active: bool,
}

impl<'a, W: io::Write> SyncUpdateGuard<'a, W> {
    fn begin(terminal: &'a mut Terminal<CrosstermBackend<W>>) -> io::Result<Self> {
        queue!(terminal.backend_mut(), BeginSynchronizedUpdate)?;
        Ok(Self {
            terminal,
            active: true,
        })
    }

    fn finish(mut self) -> io::Result<()> {
        self.active = false;
        execute!(self.terminal.backend_mut(), EndSynchronizedUpdate)
    }
}

impl<W: io::Write> Drop for SyncUpdateGuard<'_, W> {
    fn drop(&mut self) {
        if self.active {
            let _ = execute!(self.terminal.backend_mut(), EndSynchronizedUpdate);
        }
    }
}

/// Clears the entire alternate screen and resets both ratatui buffers without
/// querying the terminal cursor position (`\x1b[6n`).
///
/// Unlike `Terminal::clear()`, which calls `crossterm::cursor::position()` and
/// can block or time out inside DEC Mode 2026 synchronized updates or while the
/// terminal is decoding large graphics frames, this emits `\x1b[2J` directly
/// and forces the next `Terminal::draw()` to repaint every cell.
fn clear_fullscreen_without_query<W: io::Write>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
) -> io::Result<()> {
    terminal.backend_mut().clear_region(ClearType::All)?;
    terminal.current_buffer_mut().reset();
    terminal.swap_buffers();
    Ok(())
}

/// Runs a terminal clear/draw closure inside a DEC Mode 2026 synchronized update
/// block so the terminal presents the complete frame atomically.
fn run_synchronized_update<W, F>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    f: F,
) -> io::Result<()>
where
    W: io::Write,
    F: FnOnce(&mut Terminal<CrosstermBackend<W>>) -> io::Result<()>,
{
    let guard = SyncUpdateGuard::begin(terminal)?;
    f(guard.terminal)?;
    guard.finish()
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Parse CLI arguments first to handle help/invalid options without blocking stdin
    let options = parse_cli_args()?;
    let initial_path = options.initial_path;
    let initial_filter = options.filter;
    let scale_mode = options.scale;
    let slideshow_opt = options.slideshow;
    let check_magic = options.check_magic;

    // 2. Check if we have piped input via stdin (e.g. from fd or find)
    let piped_files = read_piped_stdin();
    let is_piped = !piped_files.is_empty();

    if is_piped && initial_path.is_some() {
        return Err(
            "Error: Positional path argument cannot be specified when piping input from stdin"
                .into(),
        );
    }

    // Validate recursive scanning option
    if options.recursive {
        if is_piped {
            return Err(
                "Error: Option --recursive (-R) cannot be used with piped stdin input".into(),
            );
        }
        if let Some(ref path) = initial_path {
            if is_cbz_or_zip(path, check_magic) {
                return Err(
                    "Error: Option --recursive (-R) is not applicable to zip/cbz archives".into(),
                );
            }
            if path.is_file() {
                return Err(
                    "Error: Option --recursive (-R) is not applicable when specifying a file path"
                        .into(),
                );
            }
        }
    }

    let initial_scan_path = initial_path.unwrap_or_else(|| PathBuf::from("."));
    let is_cbz = !is_piped && is_cbz_or_zip(&initial_scan_path, check_magic);

    // Get the image file list and current starting index
    let (images, current_index) = if is_piped {
        let sources = collect_sources(&piped_files, check_magic)?;
        reopen_tty_stdin()?;
        (sources, 0)
    } else {
        if is_cbz {
            let pages = list_cbz_pages(&initial_scan_path)?;
            let sources = pages
                .into_iter()
                .map(|page| ImageSource::Cbz {
                    zip_path: initial_scan_path.clone(),
                    file_in_zip: page,
                })
                .collect();
            (sources, 0)
        } else {
            let (paths, index) =
                scan_directory(&initial_scan_path, check_magic, options.recursive)?;
            let sources = paths.into_iter().map(ImageSource::Local).collect();
            (sources, index)
        }
    };

    // Query terminal protocol before raw mode
    let query_options = QueryStdioOptions {
        kitty_compression: options.protocol.should_enable_kitty_compression(),
    };
    let mut picker = Picker::from_query_stdio_with_options(query_options)
        .unwrap_or_else(|_| Picker::halfblocks());
    if let Some(proto) = options.protocol.forced_protocol_type() {
        picker.set_protocol_type(proto);
    }
    let effective_compression = options
        .protocol
        .effective_compression(picker.protocol_type(), picker.supports_kitty_compression());
    let active_protocol = options
        .protocol
        .resolve_active_protocol(picker.protocol_type(), effective_compression)?;

    // Setup terminal using RAII guard
    let _guard = TerminalGuard::new()?;
    let mut active_protocol = active_protocol;
    if options.protocol.should_probe_kitty_shm()
        && matches!(
            active_protocol,
            crate::config::ActiveProtocol::Kitty(crate::config::KittyTransferMode::Direct)
        )
        && !crate::config::CompressionPolicy::is_remote_ssh_session()
    {
        let shm_ok = crate::imaging::probe_kitty_shm_support(picker.tmux_detected());
        active_protocol = active_protocol.with_kitty_shm(shm_ok);
    }
    let stdout = io::stdout();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let app_config = crate::app::AppConfig {
        filter_type: initial_filter,
        scale_mode,
        disable_thumbnail: options.no_thumbnail,
        infobar: options.infobar,
        active_protocol,
    };

    let scan_config = if is_piped {
        crate::app::ScanConfig::Piped
    } else if is_cbz {
        crate::app::ScanConfig::Cbz(initial_scan_path)
    } else {
        crate::app::ScanConfig::Directory {
            path: initial_scan_path,
            check_magic,
            recursive: options.recursive,
        }
    };

    // Create app
    let mut app = App::new(images, current_index, picker, app_config, scan_config)?;
    if let Some(state) = slideshow_opt {
        app.slideshow_state = state;
        app.slideshow_last_transition = std::time::Instant::now();
    }
    if let Some(ref path) = options.import_path {
        let is_sync = options.export_path.as_ref() == Some(path);
        if !is_sync || path.exists() {
            app.import_classifications(path).map_err(io::Error::other)?;
        }
    }

    let mut draw_needed = true;
    let mut was_showing_spinner = false;
    let mut last_loop_tick = std::time::Instant::now();
    let mut events = Vec::with_capacity(16);

    // Main event loop
    while app.running {
        let now = std::time::Instant::now();
        let delta = now.duration_since(last_loop_tick);
        last_loop_tick = now;

        if app.tick_slideshow(delta) {
            draw_needed = true;
        }

        if app.update_channels() {
            draw_needed = true;
        }
        app.tick();

        let mut term_size = terminal.size().unwrap_or_default();
        let (widget_w, widget_h) = app.get_viewport_size(term_size.width, term_size.height);
        if app.last_widget_size != (widget_w, widget_h) {
            draw_needed = true;
        }

        app.update_layout(term_size.width, term_size.height);

        if app.wait_for_frame_ready(term_size.width, term_size.height, Duration::from_millis(12)) {
            draw_needed = true;
        }

        let mut clear_before_draw = false;
        if app.needs_clear() {
            app.screen_clear.take_pending();
            clear_before_draw = app.should_clear_on_update();
            draw_needed = true;
        }

        let showing_spinner = matches!(app.display_state(), ImageDisplayState::LoadingSpinner);
        if showing_spinner != was_showing_spinner {
            was_showing_spinner = showing_spinner;
            draw_needed = true;
        }

        if draw_needed {
            draw_needed = false;
            run_synchronized_update(&mut terminal, |terminal| {
                if clear_before_draw {
                    clear_fullscreen_without_query(terminal)?;
                }
                terminal.draw(|f| {
                    let mut model = app.ui_model();
                    ui(f, &mut model);
                })?;
                Ok(())
            })?;
        }

        let poll_timeout = if app.has_pending_work() {
            Duration::from_millis(1)
        } else {
            Duration::from_millis(33)
        };
        if event::poll(poll_timeout)? {
            events.clear();
            events.push(event::read()?);
            while event::poll(Duration::from_millis(0))? {
                events.push(event::read()?);
            }

            // Only redraw on actual user inputs or window changes. Non-user events (like Kitty/Sixel
            // graphics query response escape sequences written to stdin) are processed but ignored
            // for redrawing to prevent infinite drawing feedback loops and high CPU usage.
            let mut meaningful = false;
            for ev in events.drain(..) {
                meaningful |= matches!(
                    ev,
                    event::Event::Key(_)
                        | event::Event::Resize(_, _)
                        | event::Event::Mouse(_)
                        | event::Event::Paste(_)
                );
                if let event::Event::Resize(w, h) = ev {
                    term_size.width = w;
                    term_size.height = h;
                }
                if let Some(action) = crate::commands::translate_event(app.palette_mode, &ev) {
                    app.apply_action(action, term_size.height);
                }
            }
            if events.capacity() > 64 {
                events.shrink_to(16);
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
        app.write_classifications_tsv(io::stdout().lock())?;
    }

    Ok(())
}

fn main() {
    // Register custom panic hook that restores standard terminal mode
    // before displaying panic details on console stderr.
    std::panic::set_hook(Box::new(|panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            EndSynchronizedUpdate,
            LeaveAlternateScreen,
            DisableMouseCapture
        );
        eprintln!("Application Panicked:\n{}", panic_info);
    }));

    if let Err(e) = run() {
        let err_str = e.to_string();
        if err_str.starts_with("Error: ") {
            eprintln!("{}", err_str);
        } else {
            eprintln!("Error: {}", err_str);
        }
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    use ratatui::{TerminalOptions, Viewport, layout::Rect, widgets::Paragraph};

    const BEGIN_SYNC: &[u8] = b"\x1b[?2026h";
    const END_SYNC: &[u8] = b"\x1b[?2026l";
    const CURSOR_POS_QUERY: &[u8] = b"\x1b[6n";

    #[derive(Clone, Default)]
    struct CaptureWriter {
        inner: Rc<RefCell<(Vec<u8>, usize)>>,
    }

    impl CaptureWriter {
        fn bytes(&self) -> Vec<u8> {
            self.inner.borrow().0.clone()
        }

        fn flushes(&self) -> usize {
            self.inner.borrow().1
        }

        fn clear(&self) {
            let mut inner = self.inner.borrow_mut();
            inner.0.clear();
            inner.1 = 0;
        }
    }

    impl io::Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.inner.borrow_mut().0.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.borrow_mut().1 += 1;
            Ok(())
        }
    }

    fn test_terminal(writer: CaptureWriter) -> Terminal<CrosstermBackend<CaptureWriter>> {
        let backend = CrosstermBackend::new(writer);
        Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Fixed(Rect::new(0, 0, 20, 4)),
            },
        )
        .expect("fixed-viewport terminal creation should not fail")
    }

    #[test]
    fn synchronized_update_wraps_clear_and_draw_output() {
        let capture = CaptureWriter::default();
        let mut terminal = test_terminal(capture.clone());

        // Populate ratatui's previous buffer with an initial frame so we can
        // verify that `clear_fullscreen_without_query` forces unchanged cells
        // to redraw on the next frame.
        terminal
            .draw(|f| {
                f.render_widget(Paragraph::new("hello"), f.area());
            })
            .expect("initial draw should succeed");
        capture.clear();

        run_synchronized_update(&mut terminal, |terminal| {
            clear_fullscreen_without_query(terminal)?;
            terminal.draw(|f| {
                f.render_widget(Paragraph::new("hello"), f.area());
            })?;
            Ok(())
        })
        .expect("synchronized update should succeed");

        let out = capture.bytes();
        assert!(
            capture.flushes() > 0,
            "expected writer to be flushed after synchronized update"
        );
        assert!(
            out.starts_with(BEGIN_SYNC),
            "expected output to start with BeginSynchronizedUpdate, got: {:?}",
            String::from_utf8_lossy(&out)
        );
        assert!(
            out.ends_with(END_SYNC),
            "expected output to end with EndSynchronizedUpdate, got: {:?}",
            String::from_utf8_lossy(&out)
        );
        assert!(
            !out.windows(CURSOR_POS_QUERY.len())
                .any(|w| w == CURSOR_POS_QUERY),
            "expected no cursor-position query (\\x1b[6n) during fullscreen clear, got: {:?}",
            String::from_utf8_lossy(&out)
        );

        let body = &out[BEGIN_SYNC.len()..out.len() - END_SYNC.len()];
        assert!(
            body.windows(b"\x1b[2J".len()).any(|w| w == b"\x1b[2J"),
            "expected clear escape sequence between sync markers, got: {:?}",
            String::from_utf8_lossy(body)
        );
        assert!(
            body.windows(b"hello".len()).any(|w| w == b"hello"),
            "expected unchanged cells from prior frame to redraw after clear, got: {:?}",
            String::from_utf8_lossy(body)
        );
    }

    #[test]
    fn synchronized_update_emits_end_marker_on_error() {
        let capture = CaptureWriter::default();
        let mut terminal = test_terminal(capture.clone());

        let err = run_synchronized_update(&mut terminal, |terminal| {
            terminal.backend_mut().clear()?;
            Err(io::Error::other("simulated draw failure"))
        })
        .expect_err("expected simulated error to propagate");

        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert_eq!(err.to_string(), "simulated draw failure");

        let out = capture.bytes();
        assert!(
            capture.flushes() > 0,
            "expected writer to be flushed when EndSynchronizedUpdate runs on error"
        );
        assert!(
            out.starts_with(BEGIN_SYNC),
            "expected BeginSynchronizedUpdate before failure, got: {:?}",
            String::from_utf8_lossy(&out)
        );
        assert!(
            out.ends_with(END_SYNC),
            "expected EndSynchronizedUpdate to be written on error via Drop, got: {:?}",
            String::from_utf8_lossy(&out)
        );
    }
}
