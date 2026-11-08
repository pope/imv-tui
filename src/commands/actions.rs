use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};

use super::line_editor::EditorAction;
use super::registry::Command;

/// The specific input prompt type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptType {
    /// Go to specific image index.
    GoToImage,
    /// Adjust image brightness.
    SetBrightness,
    /// Adjust image contrast.
    SetContrast,
    /// Adjust slideshow interval.
    SetSlideshow,
}

/// The state of the top overlay search palette or prompt dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteMode {
    /// Palette/prompt overlay is closed.
    Closed,
    /// Searchable commands lookup.
    Command,
    /// Fuzzy search files in local queue.
    File,
    /// Prompt value input box is open with the given prompt type.
    Prompt(PromptType),
    /// Image metadata and statistics dialog is open.
    Info,
}

/// Navigation, confirmation, and dismissal actions within an active overlay (`PaletteMode != Closed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayAction {
    /// Close the active overlay (`Esc`, or `q` when in `PaletteMode::Info`).
    Close,
    /// Confirm/submit the active palette selection or prompt input (`Enter`).
    Confirm,
    /// Move palette selection up by one row (`Up`, `Ctrl+P`, or mouse `ScrollUp`).
    SelectPrev,
    /// Move palette selection down by one row (`Down`, `Ctrl+N`, `Ctrl+J`, or mouse `ScrollDown`).
    SelectNext,
    /// Move palette selection up by one page (`PageUp` or `Alt+V`).
    SelectPageUp,
    /// Move palette selection down by one page (`PageDown` or `Ctrl+V`).
    SelectPageDown,
}

/// Unified input action produced by translating a raw Crossterm `Event` in the context of the active `PaletteMode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    /// Normal-mode application command.
    Command(Command),
    /// Text editor action when an interactive input overlay (`Command`, `File`, `Prompt`) is active.
    Editor(EditorAction),
    /// Overlay list navigation, confirmation, or dismissal action.
    Overlay(OverlayAction),
}

/// Translates a raw Crossterm `Event` into a semantic `InputAction` based on the active `PaletteMode`.
pub fn translate_event(palette_mode: PaletteMode, ev: &Event) -> Option<InputAction> {
    match palette_mode {
        PaletteMode::Info => {
            if let Event::Key(KeyEvent {
                code: KeyCode::Esc | KeyCode::Char('q'),
                kind: KeyEventKind::Press,
                ..
            }) = ev
            {
                Some(InputAction::Overlay(OverlayAction::Close))
            } else {
                Command::from_event(ev).map(InputAction::Command)
            }
        }
        PaletteMode::Command | PaletteMode::File | PaletteMode::Prompt(_) => match ev {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    return None;
                }
                if let Some(edit) = EditorAction::from_key_event(key) {
                    return Some(InputAction::Editor(edit));
                }
                match key.code {
                    KeyCode::Esc => Some(InputAction::Overlay(OverlayAction::Close)),
                    KeyCode::Enter => Some(InputAction::Overlay(OverlayAction::Confirm)),
                    KeyCode::Up | KeyCode::Char('p')
                        if key.code == KeyCode::Up
                            || key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        Some(InputAction::Overlay(OverlayAction::SelectPrev))
                    }
                    KeyCode::Down | KeyCode::Char('n') | KeyCode::Char('j')
                        if key.code == KeyCode::Down
                            || key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        Some(InputAction::Overlay(OverlayAction::SelectNext))
                    }
                    KeyCode::PageUp | KeyCode::Char('v')
                        if key.code == KeyCode::PageUp
                            || (key.code == KeyCode::Char('v')
                                && key.modifiers.contains(KeyModifiers::ALT)) =>
                    {
                        Some(InputAction::Overlay(OverlayAction::SelectPageUp))
                    }
                    KeyCode::PageDown | KeyCode::Char('v')
                        if key.code == KeyCode::PageDown
                            || (key.code == KeyCode::Char('v')
                                && key.modifiers.contains(KeyModifiers::CONTROL)) =>
                    {
                        Some(InputAction::Overlay(OverlayAction::SelectPageDown))
                    }
                    _ => None,
                }
            }
            Event::Mouse(mouse_event) => match mouse_event.kind {
                MouseEventKind::ScrollUp => Some(InputAction::Overlay(OverlayAction::SelectPrev)),
                MouseEventKind::ScrollDown => Some(InputAction::Overlay(OverlayAction::SelectNext)),
                _ => None,
            },
            _ => None,
        },
        PaletteMode::Closed => Command::from_event(ev).map(InputAction::Command),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyEventState, MouseButton, MouseEvent};

    fn make_key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        })
    }

    #[test]
    fn test_translate_event_closed() {
        assert_eq!(
            translate_event(
                PaletteMode::Closed,
                &make_key(KeyCode::Char('q'), KeyModifiers::NONE)
            ),
            Some(InputAction::Command(Command::Quit))
        );

        let release_ev = Event::Key(KeyEvent {
            code: KeyCode::Char('q'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: KeyEventState::NONE,
        });
        assert_eq!(translate_event(PaletteMode::Closed, &release_ev), None);
    }

    #[test]
    fn test_translate_event_info() {
        assert_eq!(
            translate_event(
                PaletteMode::Info,
                &make_key(KeyCode::Esc, KeyModifiers::NONE)
            ),
            Some(InputAction::Overlay(OverlayAction::Close))
        );
        assert_eq!(
            translate_event(
                PaletteMode::Info,
                &make_key(KeyCode::Char('q'), KeyModifiers::NONE)
            ),
            Some(InputAction::Overlay(OverlayAction::Close))
        );
        assert_eq!(
            translate_event(
                PaletteMode::Info,
                &make_key(KeyCode::Char('d'), KeyModifiers::NONE)
            ),
            Some(InputAction::Command(Command::ShowInfo))
        );
    }

    #[test]
    fn test_translate_event_palette_editor_and_overlay() {
        let mode = PaletteMode::Command;

        // Editor actions
        assert_eq!(
            translate_event(mode, &make_key(KeyCode::Char('a'), KeyModifiers::NONE)),
            Some(InputAction::Editor(EditorAction::InsertChar('a')))
        );
        assert_eq!(
            translate_event(mode, &make_key(KeyCode::Backspace, KeyModifiers::NONE)),
            Some(InputAction::Editor(EditorAction::Backspace))
        );
        assert_eq!(
            translate_event(mode, &make_key(KeyCode::Left, KeyModifiers::NONE)),
            Some(InputAction::Editor(EditorAction::MoveLeft))
        );

        // Overlay actions
        assert_eq!(
            translate_event(mode, &make_key(KeyCode::Up, KeyModifiers::NONE)),
            Some(InputAction::Overlay(OverlayAction::SelectPrev))
        );
        assert_eq!(
            translate_event(mode, &make_key(KeyCode::Char('p'), KeyModifiers::CONTROL)),
            Some(InputAction::Overlay(OverlayAction::SelectPrev))
        );
        assert_eq!(
            translate_event(mode, &make_key(KeyCode::Down, KeyModifiers::NONE)),
            Some(InputAction::Overlay(OverlayAction::SelectNext))
        );
        assert_eq!(
            translate_event(mode, &make_key(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            Some(InputAction::Overlay(OverlayAction::SelectNext))
        );
        assert_eq!(
            translate_event(mode, &make_key(KeyCode::Char('j'), KeyModifiers::CONTROL)),
            Some(InputAction::Overlay(OverlayAction::SelectNext))
        );
        assert_eq!(
            translate_event(mode, &make_key(KeyCode::PageUp, KeyModifiers::NONE)),
            Some(InputAction::Overlay(OverlayAction::SelectPageUp))
        );
        assert_eq!(
            translate_event(mode, &make_key(KeyCode::Char('v'), KeyModifiers::ALT)),
            Some(InputAction::Overlay(OverlayAction::SelectPageUp))
        );
        assert_eq!(
            translate_event(mode, &make_key(KeyCode::PageDown, KeyModifiers::NONE)),
            Some(InputAction::Overlay(OverlayAction::SelectPageDown))
        );
        assert_eq!(
            translate_event(mode, &make_key(KeyCode::Char('v'), KeyModifiers::CONTROL)),
            Some(InputAction::Overlay(OverlayAction::SelectPageDown))
        );
        assert_eq!(
            translate_event(mode, &make_key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(InputAction::Overlay(OverlayAction::Confirm))
        );
        assert_eq!(
            translate_event(mode, &make_key(KeyCode::Esc, KeyModifiers::NONE)),
            Some(InputAction::Overlay(OverlayAction::Close))
        );

        // Mouse events
        let scroll_up = Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(
            translate_event(mode, &scroll_up),
            Some(InputAction::Overlay(OverlayAction::SelectPrev))
        );

        let scroll_down = Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(
            translate_event(mode, &scroll_down),
            Some(InputAction::Overlay(OverlayAction::SelectNext))
        );

        let click = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(translate_event(mode, &click), None);
    }

    #[test]
    fn test_translate_event_file_and_prompt() {
        for mode in [
            PaletteMode::File,
            PaletteMode::Prompt(PromptType::GoToImage),
            PaletteMode::Prompt(PromptType::SetBrightness),
        ] {
            assert_eq!(
                translate_event(mode, &make_key(KeyCode::Char('x'), KeyModifiers::NONE)),
                Some(InputAction::Editor(EditorAction::InsertChar('x')))
            );
            assert_eq!(
                translate_event(mode, &make_key(KeyCode::Enter, KeyModifiers::NONE)),
                Some(InputAction::Overlay(OverlayAction::Confirm))
            );
            assert_eq!(
                translate_event(mode, &make_key(KeyCode::Esc, KeyModifiers::NONE)),
                Some(InputAction::Overlay(OverlayAction::Close))
            );
        }
    }
}
