use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};

use super::line_editor::EditorAction;
use super::registry::{Command, PanDirection};

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
    /// Release of a directional pan key (when Kitty keyboard event types are active).
    PanRelease(PanDirection),
    /// Clears all tracked held pan keys (e.g. when the terminal loses focus).
    ClearHeldKeys,
}

/// Translates a raw Crossterm `Event` into a semantic `InputAction` based on the active `PaletteMode`.
pub fn translate_event(palette_mode: PaletteMode, ev: &Event) -> Option<InputAction> {
    if matches!(ev, Event::FocusLost) {
        return Some(InputAction::ClearHeldKeys);
    }

    if let Event::Key(key) = ev
        && key.kind == KeyEventKind::Release
    {
        let dir = match key.code {
            KeyCode::Left | KeyCode::Char('h' | 'H') => Some(PanDirection::Left),
            KeyCode::Right | KeyCode::Char('l' | 'L') => Some(PanDirection::Right),
            KeyCode::Up | KeyCode::Char('k' | 'K') => Some(PanDirection::Up),
            KeyCode::Down | KeyCode::Char('j' | 'J') => Some(PanDirection::Down),
            _ => None,
        };
        return dir.map(InputAction::PanRelease);
    }

    match palette_mode {
        PaletteMode::Info => {
            if let Event::Key(KeyEvent {
                code: KeyCode::Esc | KeyCode::Char('q'),
                kind: KeyEventKind::Press | KeyEventKind::Repeat,
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
                if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
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

    #[test]
    fn test_translate_event_pan_release_and_focus_lost() {
        let make_release = |code: KeyCode, modifiers: KeyModifiers| {
            Event::Key(KeyEvent {
                code,
                modifiers,
                kind: KeyEventKind::Release,
                state: KeyEventState::NONE,
            })
        };

        let all_modes = [
            PaletteMode::Closed,
            PaletteMode::Command,
            PaletteMode::File,
            PaletteMode::Prompt(PromptType::GoToImage),
            PaletteMode::Info,
        ];

        for mode in all_modes {
            // FocusLost clears held keys in every mode
            assert_eq!(
                translate_event(mode, &Event::FocusLost),
                Some(InputAction::ClearHeldKeys)
            );

            // Arrow and hjkl releases (including uppercase/Shift) emit PanRelease
            assert_eq!(
                translate_event(mode, &make_release(KeyCode::Left, KeyModifiers::NONE)),
                Some(InputAction::PanRelease(PanDirection::Left))
            );
            assert_eq!(
                translate_event(mode, &make_release(KeyCode::Char('h'), KeyModifiers::NONE)),
                Some(InputAction::PanRelease(PanDirection::Left))
            );
            assert_eq!(
                translate_event(mode, &make_release(KeyCode::Char('H'), KeyModifiers::SHIFT)),
                Some(InputAction::PanRelease(PanDirection::Left))
            );
            assert_eq!(
                translate_event(mode, &make_release(KeyCode::Right, KeyModifiers::NONE)),
                Some(InputAction::PanRelease(PanDirection::Right))
            );
            assert_eq!(
                translate_event(mode, &make_release(KeyCode::Char('l'), KeyModifiers::NONE)),
                Some(InputAction::PanRelease(PanDirection::Right))
            );
            assert_eq!(
                translate_event(mode, &make_release(KeyCode::Char('L'), KeyModifiers::SHIFT)),
                Some(InputAction::PanRelease(PanDirection::Right))
            );
            assert_eq!(
                translate_event(mode, &make_release(KeyCode::Up, KeyModifiers::NONE)),
                Some(InputAction::PanRelease(PanDirection::Up))
            );
            assert_eq!(
                translate_event(mode, &make_release(KeyCode::Char('k'), KeyModifiers::NONE)),
                Some(InputAction::PanRelease(PanDirection::Up))
            );
            assert_eq!(
                translate_event(mode, &make_release(KeyCode::Char('K'), KeyModifiers::SHIFT)),
                Some(InputAction::PanRelease(PanDirection::Up))
            );
            assert_eq!(
                translate_event(mode, &make_release(KeyCode::Down, KeyModifiers::NONE)),
                Some(InputAction::PanRelease(PanDirection::Down))
            );
            assert_eq!(
                translate_event(mode, &make_release(KeyCode::Char('j'), KeyModifiers::NONE)),
                Some(InputAction::PanRelease(PanDirection::Down))
            );
            assert_eq!(
                translate_event(mode, &make_release(KeyCode::Char('J'), KeyModifiers::SHIFT)),
                Some(InputAction::PanRelease(PanDirection::Down))
            );

            // Ctrl/Alt modified releases still clear held pan state
            assert_eq!(
                translate_event(mode, &make_release(KeyCode::Left, KeyModifiers::CONTROL)),
                Some(InputAction::PanRelease(PanDirection::Left))
            );
            assert_eq!(
                translate_event(mode, &make_release(KeyCode::Char('h'), KeyModifiers::ALT)),
                Some(InputAction::PanRelease(PanDirection::Left))
            );

            // Non-pan key releases are ignored
            assert_eq!(
                translate_event(mode, &make_release(KeyCode::Char('q'), KeyModifiers::NONE)),
                None
            );
        }
    }

    #[test]
    fn test_translate_event_repeat_events() {
        let make_repeat = |code: KeyCode, modifiers: KeyModifiers| {
            Event::Key(KeyEvent {
                code,
                modifiers,
                kind: KeyEventKind::Repeat,
                state: KeyEventState::NONE,
            })
        };

        // Normal mode command repeat
        assert_eq!(
            translate_event(
                PaletteMode::Closed,
                &make_repeat(KeyCode::Left, KeyModifiers::NONE)
            ),
            Some(InputAction::Command(Command::PanLeft))
        );
        assert_eq!(
            translate_event(
                PaletteMode::Closed,
                &make_repeat(KeyCode::Char('j'), KeyModifiers::NONE)
            ),
            Some(InputAction::Command(Command::PanDown))
        );

        // Palette editor & overlay repeat
        assert_eq!(
            translate_event(
                PaletteMode::Command,
                &make_repeat(KeyCode::Backspace, KeyModifiers::NONE)
            ),
            Some(InputAction::Editor(EditorAction::Backspace))
        );
        assert_eq!(
            translate_event(
                PaletteMode::Command,
                &make_repeat(KeyCode::Down, KeyModifiers::NONE)
            ),
            Some(InputAction::Overlay(OverlayAction::SelectNext))
        );
        assert_eq!(
            translate_event(
                PaletteMode::File,
                &make_repeat(KeyCode::Up, KeyModifiers::NONE)
            ),
            Some(InputAction::Overlay(OverlayAction::SelectPrev))
        );
    }
}
