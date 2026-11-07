use super::PaletteMode;
use crate::app::App;
use crate::commands::Command;
use crossterm::event::{Event, KeyCode, KeyEventKind};

impl App {
    /// Handles a Crossterm input event (keyboard or mouse).
    pub fn handle_event(&mut self, ev: Event, terminal_height: u16) {
        if let (
            PaletteMode::Info,
            Event::Key(crossterm::event::KeyEvent {
                code,
                kind: KeyEventKind::Press,
                ..
            }),
        ) = (self.palette_mode, &ev)
        {
            match code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.palette_mode = PaletteMode::Closed;
                    self.needs_clear = true;
                    return;
                }
                KeyCode::Char('d') => {
                    if self
                        .last_info_toggle
                        .is_none_or(|t| t.elapsed() > std::time::Duration::from_millis(200))
                    {
                        self.palette_mode = PaletteMode::Closed;
                        self.needs_clear = true;
                        self.last_info_toggle = Some(std::time::Instant::now());
                    }
                    return;
                }
                _ => {}
            }
        }

        if self.palette_mode != PaletteMode::Closed && self.palette_mode != PaletteMode::Info {
            if let Event::Key(key) = ev {
                if key.kind != KeyEventKind::Press {
                    return;
                }

                // Handle editor keys first
                match self.palette_query.handle_key_event(&key) {
                    super::line_editor::EditorResult::ConsumedChanged => {
                        self.palette_selected_index = 0;
                        self.update_palette_filter();
                        return;
                    }
                    super::line_editor::EditorResult::ConsumedNoChange => {
                        return;
                    }
                    super::line_editor::EditorResult::NotConsumed => {}
                }

                match key.code {
                    KeyCode::Esc => {
                        self.palette_mode = PaletteMode::Closed;
                        self.needs_update = true;
                        self.needs_clear = true;
                    }
                    KeyCode::Enter => match self.palette_mode {
                        PaletteMode::File => {
                            let files = self.get_filtered_files();
                            if !files.is_empty() && self.palette_selected_index < files.len() {
                                self.queue.current_index = files[self.palette_selected_index].0;
                                self.start_load_image();
                            }
                            self.palette_mode = PaletteMode::Closed;
                            self.needs_update = true;
                            self.needs_clear = true;
                        }
                        PaletteMode::Command => {
                            let cmds = self.get_filtered_commands();
                            if !cmds.is_empty() && self.palette_selected_index < cmds.len() {
                                let cmd = cmds[self.palette_selected_index].cmd;
                                self.execute_command(cmd);
                            }
                            if self.palette_mode == PaletteMode::Command {
                                self.palette_mode = PaletteMode::Closed;
                                self.needs_update = true;
                                self.needs_clear = true;
                            }
                        }
                        PaletteMode::Prompt => {
                            if let Some(prompt_type) = self.prompt_type {
                                self.execute_prompt(prompt_type);
                            }
                        }
                        _ => {}
                    },
                    // List Navigation: Up / Ctrl+P
                    KeyCode::Up | KeyCode::Char('p')
                        if (key.code == KeyCode::Up
                            || key
                                .modifiers
                                .contains(crossterm::event::KeyModifiers::CONTROL))
                            && self.palette_selected_index > 0 =>
                    {
                        self.palette_selected_index -= 1;
                    }
                    // List Navigation: Down / Ctrl+N / Ctrl+J
                    KeyCode::Down | KeyCode::Char('n') | KeyCode::Char('j')
                        if key.code == KeyCode::Down
                            || key
                                .modifiers
                                .contains(crossterm::event::KeyModifiers::CONTROL) =>
                    {
                        let max_len = match self.palette_mode {
                            PaletteMode::File => self.get_filtered_files().len(),
                            PaletteMode::Command => self.get_filtered_commands().len(),
                            _ => 0,
                        };
                        if max_len > 0 && self.palette_selected_index < max_len - 1 {
                            self.palette_selected_index += 1;
                        }
                    }
                    KeyCode::PageUp | KeyCode::Char('v')
                        if key.code == KeyCode::PageUp
                            || (key.code == KeyCode::Char('v')
                                && key.modifiers.contains(crossterm::event::KeyModifiers::ALT)) =>
                    {
                        let max_len = match self.palette_mode {
                            PaletteMode::File => self.get_filtered_files().len(),
                            PaletteMode::Command => self.get_filtered_commands().len(),
                            _ => 0,
                        };
                        let viewport_h = terminal_height.saturating_sub(1);
                        let max_h = (viewport_h as f64 * 0.5).round() as u16;
                        let palette_h = (max_len as u16 + 4).max(12).min(max_h);
                        let page_size = (palette_h as usize).saturating_sub(4);

                        self.palette_selected_index =
                            self.palette_selected_index.saturating_sub(page_size);
                    }
                    KeyCode::PageDown | KeyCode::Char('v')
                        if key.code == KeyCode::PageDown
                            || (key.code == KeyCode::Char('v')
                                && key
                                    .modifiers
                                    .contains(crossterm::event::KeyModifiers::CONTROL)) =>
                    {
                        let max_len = match self.palette_mode {
                            PaletteMode::File => self.get_filtered_files().len(),
                            PaletteMode::Command => self.get_filtered_commands().len(),
                            _ => 0,
                        };
                        if max_len > 0 {
                            let viewport_h = terminal_height.saturating_sub(1);
                            let max_h = (viewport_h as f64 * 0.5).round() as u16;
                            let palette_h = (max_len as u16 + 4).max(12).min(max_h);
                            let page_size = (palette_h as usize).saturating_sub(4);

                            self.palette_selected_index =
                                (self.palette_selected_index + page_size).min(max_len - 1);
                        }
                    }
                    _ => {}
                }
            } else if let Event::Mouse(mouse_event) = ev {
                match mouse_event.kind {
                    crossterm::event::MouseEventKind::ScrollUp
                        if self.palette_selected_index > 0 =>
                    {
                        self.palette_selected_index -= 1;
                    }
                    crossterm::event::MouseEventKind::ScrollDown => {
                        let max_len = match self.palette_mode {
                            PaletteMode::File => self.get_filtered_files().len(),
                            PaletteMode::Command => self.get_filtered_commands().len(),
                            _ => 0,
                        };
                        if max_len > 0 && self.palette_selected_index < max_len - 1 {
                            self.palette_selected_index += 1;
                        }
                    }
                    _ => {}
                }
            }
        } else {
            if let Some(cmd) = Command::from_event(&ev) {
                self.execute_command(cmd);
            }
        }
    }

    /// Handles a Crossterm keyboard event.
    pub(crate) fn handle_key_event(
        &mut self,
        key: crossterm::event::KeyEvent,
        terminal_height: u16,
    ) {
        self.handle_event(Event::Key(key), terminal_height);
    }

    /// Handles a Crossterm mouse event.
    pub(crate) fn handle_mouse_event(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        terminal_height: u16,
    ) {
        self.handle_event(Event::Mouse(mouse), terminal_height);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::PromptType;
    use crate::app::classifications::Classification;
    use crate::app::setup_test_app;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MouseEvent, MouseEventKind,
    };

    fn make_key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn make_mouse(kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column: 10,
            row: 10,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn test_normal_mode_key_dispatch() {
        let mut app = setup_test_app();
        app.original_image = Some(std::sync::Arc::new(image::DynamicImage::ImageRgb8(
            image::RgbImage::new(800, 600),
        )));
        app.is_loading = false;
        app.img_width = 800;
        app.img_height = 600;
        app.last_widget_size = (80, 24);

        // Zoom in '+'
        let initial_zoom = app.zoom_factor.value();
        app.handle_key_event(make_key(KeyCode::Char('+'), KeyModifiers::NONE), 24);
        assert!(app.zoom_factor.value() > initial_zoom);

        // Zoom out '-'
        app.handle_key_event(make_key(KeyCode::Char('-'), KeyModifiers::NONE), 24);
        assert!((app.zoom_factor.value() - initial_zoom).abs() < 1e-4);

        // Panning 'l' (right) and 'h' (left)
        app.handle_key_event(make_key(KeyCode::Char('l'), KeyModifiers::NONE), 24);
        assert!(app.pan_offset.x > 0);
        app.handle_key_event(make_key(KeyCode::Char('r'), KeyModifiers::NONE), 24);
        assert_eq!(app.pan_offset.x, 0);

        // Classifications 'z' (pick), 'x' (reject), 'u' (unflag)
        app.handle_key_event(make_key(KeyCode::Char('z'), KeyModifiers::NONE), 24);
        assert_eq!(app.current_classification(), Classification::Pick);
        // Marking as reject hides it from default view and advances to next visible image
        app.handle_key_event(make_key(KeyCode::Char('x'), KeyModifiers::NONE), 24);
        assert_eq!(app.classifications()[0], Classification::Reject);
        app.handle_key_event(make_key(KeyCode::Char('u'), KeyModifiers::NONE), 24);
        assert_eq!(app.current_classification(), Classification::Unflagged);

        // Overlays: ':' opens Command Palette
        app.handle_key_event(make_key(KeyCode::Char(':'), KeyModifiers::NONE), 24);
        assert_eq!(app.palette_mode, PaletteMode::Command);
        app.palette_mode = PaletteMode::Closed;

        // 'f' opens File search
        app.handle_key_event(make_key(KeyCode::Char('f'), KeyModifiers::NONE), 24);
        assert_eq!(app.palette_mode, PaletteMode::File);
        app.palette_mode = PaletteMode::Closed;

        // Command::GoToImage opens Goto prompt
        app.execute_command(Command::GoToImage);
        assert_eq!(app.palette_mode, PaletteMode::Prompt);
        assert_eq!(app.prompt_type, Some(PromptType::GoToImage));
        app.palette_mode = PaletteMode::Closed;

        // 'd' opens Info dialog
        app.handle_key_event(make_key(KeyCode::Char('d'), KeyModifiers::NONE), 24);
        assert_eq!(app.palette_mode, PaletteMode::Info);
        app.palette_mode = PaletteMode::Closed;

        // Reset classification and index for navigation testing
        app.queue.set_classification(0, Classification::Unflagged);
        app.queue.current_index = 0;

        // 'n' next image
        assert_eq!(app.queue.current_index, 0);
        app.handle_key_event(make_key(KeyCode::Char('n'), KeyModifiers::NONE), 24);
        assert_eq!(app.queue.current_index, 1);

        // 'p' previous image
        app.handle_key_event(make_key(KeyCode::Char('p'), KeyModifiers::NONE), 24);
        assert_eq!(app.queue.current_index, 0);

        // 'q' quits
        app.handle_key_event(make_key(KeyCode::Char('q'), KeyModifiers::NONE), 24);
        assert!(!app.running);
    }

    #[test]
    fn test_palette_mode_event_dispatch() {
        let mut app = setup_test_app();
        app.open_palette(PaletteMode::Command);
        assert_eq!(app.palette_mode, PaletteMode::Command);

        // Typing query into line editor
        app.handle_key_event(make_key(KeyCode::Char('q'), KeyModifiers::NONE), 24);
        app.handle_key_event(make_key(KeyCode::Char('u'), KeyModifiers::NONE), 24);
        app.handle_key_event(make_key(KeyCode::Char('i'), KeyModifiers::NONE), 24);
        app.handle_key_event(make_key(KeyCode::Char('t'), KeyModifiers::NONE), 24);
        assert_eq!(app.palette_query.value(), "quit");

        // List navigation Down / Up
        let init_idx = app.palette_selected_index;
        app.handle_key_event(make_key(KeyCode::Down, KeyModifiers::NONE), 24);
        if app.get_filtered_commands().len() > 1 {
            assert_eq!(app.palette_selected_index, init_idx + 1);
            app.handle_key_event(make_key(KeyCode::Up, KeyModifiers::NONE), 24);
            assert_eq!(app.palette_selected_index, init_idx);
        }

        // Mouse scrolling in palette mode
        let max_items = app.get_filtered_commands().len();
        if max_items > 1 {
            app.handle_mouse_event(make_mouse(MouseEventKind::ScrollDown), 24);
            assert!(app.palette_selected_index > 0);
            app.handle_mouse_event(make_mouse(MouseEventKind::ScrollUp), 24);
            assert_eq!(app.palette_selected_index, 0);
        }

        // Esc closes palette
        app.handle_key_event(make_key(KeyCode::Esc, KeyModifiers::NONE), 24);
        assert_eq!(app.palette_mode, PaletteMode::Closed);
    }

    #[test]
    fn test_info_overlay_and_mouse_zoom() {
        let mut app = setup_test_app();
        app.original_image = Some(std::sync::Arc::new(image::DynamicImage::ImageRgb8(
            image::RgbImage::new(800, 600),
        )));
        app.is_loading = false;
        app.img_width = 800;
        app.img_height = 600;
        app.last_widget_size = (80, 24);

        // Info mode Esc and 'q' close
        app.palette_mode = PaletteMode::Info;
        app.handle_key_event(make_key(KeyCode::Esc, KeyModifiers::NONE), 24);
        assert_eq!(app.palette_mode, PaletteMode::Closed);

        app.palette_mode = PaletteMode::Info;
        app.handle_key_event(make_key(KeyCode::Char('q'), KeyModifiers::NONE), 24);
        assert_eq!(app.palette_mode, PaletteMode::Closed);

        // Mouse wheel zooming in normal mode
        let init_zoom = app.zoom_factor.value();
        app.handle_mouse_event(make_mouse(MouseEventKind::ScrollUp), 24);
        assert!(app.zoom_factor.value() > init_zoom);

        app.handle_mouse_event(make_mouse(MouseEventKind::ScrollDown), 24);
        assert!((app.zoom_factor.value() - init_zoom).abs() < 1e-4);
    }

    #[test]
    fn test_prompt_event_dispatch() {
        let mut app = setup_test_app();

        // 1. Open GoToImage prompt
        app.open_prompt(PromptType::GoToImage);
        assert_eq!(app.palette_mode, PaletteMode::Prompt);
        assert_eq!(app.prompt_type, Some(PromptType::GoToImage));

        // 2. Type '2'
        app.handle_key_event(make_key(KeyCode::Char('2'), KeyModifiers::NONE), 24);
        assert_eq!(app.palette_query.value(), "2");

        // 3. Press Enter -> execute prompt and close
        app.handle_key_event(make_key(KeyCode::Enter, KeyModifiers::NONE), 24);
        assert_eq!(app.palette_mode, PaletteMode::Closed);
        assert_eq!(app.prompt_type, None);
        assert_eq!(app.queue.current_index, 1);

        // 4. Open prompt again and cancel with Esc
        app.open_prompt(PromptType::GoToImage);
        app.handle_key_event(make_key(KeyCode::Char('1'), KeyModifiers::NONE), 24);
        assert_eq!(app.palette_query.value(), "1");
        app.handle_key_event(make_key(KeyCode::Esc, KeyModifiers::NONE), 24);
        assert_eq!(app.palette_mode, PaletteMode::Closed);
        // Current index should remain 1, not cancelled target 0
        assert_eq!(app.queue.current_index, 1);
    }
}
