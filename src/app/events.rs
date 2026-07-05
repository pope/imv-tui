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
                    self.needs_clear_once = true;
                    return;
                }
                KeyCode::Char('d') => {
                    if self.last_info_toggle.is_none()
                        || self.last_info_toggle.unwrap().elapsed()
                            > std::time::Duration::from_millis(200)
                    {
                        self.palette_mode = PaletteMode::Closed;
                        self.needs_clear_once = true;
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
                match key.code {
                    KeyCode::Esc => {
                        self.palette_mode = PaletteMode::Closed;
                        self.needs_update = true;
                        self.needs_clear_once = true;
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
                            self.needs_clear_once = true;
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
                                self.needs_clear_once = true;
                            }
                        }
                        PaletteMode::Prompt => {
                            if let Some(prompt_type) = self.prompt_type {
                                self.execute_prompt(prompt_type);
                            }
                        }
                        _ => {}
                    },
                    KeyCode::Up if self.palette_selected_index > 0 => {
                        self.palette_selected_index -= 1;
                    }
                    KeyCode::Down => {
                        let max_len = match self.palette_mode {
                            PaletteMode::File => self.get_filtered_files().len(),
                            PaletteMode::Command => self.get_filtered_commands().len(),
                            _ => 0,
                        };
                        if max_len > 0 && self.palette_selected_index < max_len - 1 {
                            self.palette_selected_index += 1;
                        }
                    }
                    KeyCode::PageUp => {
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
                    KeyCode::PageDown => {
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
                    KeyCode::Char('k')
                        if key
                            .modifiers
                            .contains(crossterm::event::KeyModifiers::CONTROL)
                            && self.palette_selected_index > 0 =>
                    {
                        self.palette_selected_index -= 1;
                    }
                    KeyCode::Char('j')
                        if key
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
                    KeyCode::Backspace => {
                        self.palette_pop_char();
                    }
                    KeyCode::Char(c) => {
                        self.palette_push_char(c);
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
}
