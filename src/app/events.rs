use super::PaletteMode;
use crate::app::App;
#[cfg(test)]
use crate::commands::translate_event;
use crate::commands::{InputAction, OverlayAction};
#[cfg(test)]
use crossterm::event::Event;

impl App {
    /// Pure reducer executing a translated `InputAction`.
    pub fn apply_action(&mut self, action: InputAction, terminal_height: u16) {
        match action {
            InputAction::Command(cmd) => {
                self.execute_command(cmd);
            }
            InputAction::Editor(edit) => {
                if self.palette_query.apply_action(edit) {
                    self.palette_selected_index = 0;
                    self.update_palette_filter();
                }
            }
            InputAction::Overlay(overlay) => match overlay {
                OverlayAction::Close => {
                    self.close_palette();
                }
                OverlayAction::Confirm => match self.palette_mode {
                    PaletteMode::File => {
                        let files = self.get_filtered_files();
                        if !files.is_empty() && self.palette_selected_index < files.len() {
                            self.queue.current_index = files[self.palette_selected_index];
                            self.start_load_image();
                        }
                        self.close_palette();
                    }
                    PaletteMode::Command => {
                        let cmds = self.get_filtered_commands();
                        if !cmds.is_empty() && self.palette_selected_index < cmds.len() {
                            let cmd = crate::commands::get_commands()
                                [cmds[self.palette_selected_index]]
                                .cmd;
                            self.execute_command(cmd);
                        }
                        if self.palette_mode == PaletteMode::Command {
                            self.close_palette();
                        }
                    }
                    PaletteMode::Prompt(prompt_type) => {
                        self.execute_prompt(prompt_type);
                    }
                    _ => {}
                },
                OverlayAction::SelectPrev => {
                    if self.palette_selected_index > 0 {
                        self.palette_selected_index -= 1;
                    }
                }
                OverlayAction::SelectNext => {
                    let max_len = match self.palette_mode {
                        PaletteMode::File => self.get_filtered_files().len(),
                        PaletteMode::Command => self.get_filtered_commands().len(),
                        _ => 0,
                    };
                    if max_len > 0 && self.palette_selected_index < max_len - 1 {
                        self.palette_selected_index += 1;
                    }
                }
                OverlayAction::SelectPageUp => {
                    let max_len = match self.palette_mode {
                        PaletteMode::File => self.get_filtered_files().len(),
                        PaletteMode::Command => self.get_filtered_commands().len(),
                        _ => 0,
                    };
                    let page_size = super::palette::palette_page_size(terminal_height, max_len);

                    self.palette_selected_index =
                        self.palette_selected_index.saturating_sub(page_size);
                }
                OverlayAction::SelectPageDown => {
                    let max_len = match self.palette_mode {
                        PaletteMode::File => self.get_filtered_files().len(),
                        PaletteMode::Command => self.get_filtered_commands().len(),
                        _ => 0,
                    };
                    if max_len > 0 {
                        let page_size = super::palette::palette_page_size(terminal_height, max_len);

                        self.palette_selected_index =
                            (self.palette_selected_index + page_size).min(max_len - 1);
                    }
                }
            },
        }
    }

    #[cfg(test)]
    /// Handles a Crossterm input event (keyboard or mouse).
    pub fn handle_event(&mut self, ev: Event, terminal_height: u16) {
        if let Some(action) = translate_event(self.palette_mode, &ev) {
            self.apply_action(action, terminal_height);
        }
    }

    #[cfg(test)]
    /// Handles a Crossterm keyboard event.
    pub fn handle_key_event(&mut self, key: crossterm::event::KeyEvent, terminal_height: u16) {
        self.handle_event(Event::Key(key), terminal_height);
    }

    #[cfg(test)]
    /// Handles a Crossterm mouse event.
    pub fn handle_mouse_event(
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
    use crate::commands::Command;
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
        app.initial_load = crate::app::InitialLoadState::Idle;
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
        assert_eq!(app.palette_mode, PaletteMode::Prompt(PromptType::GoToImage));
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
        app.initial_load = crate::app::InitialLoadState::Idle;
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
        assert_eq!(app.palette_mode, PaletteMode::Prompt(PromptType::GoToImage));

        // 2. Type '2'
        app.handle_key_event(make_key(KeyCode::Char('2'), KeyModifiers::NONE), 24);
        assert_eq!(app.palette_query.value(), "2");

        // 3. Press Enter -> execute prompt and close
        app.handle_key_event(make_key(KeyCode::Enter, KeyModifiers::NONE), 24);
        assert_eq!(app.palette_mode, PaletteMode::Closed);
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
