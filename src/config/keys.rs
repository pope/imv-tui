use crossterm::event;

/// Abstract representation of keyboard keys and mouse actions for bindings.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyDef {
    /// A single character press (e.g. 'q', 'i').
    Char(char),
    /// A standalone keyboard key (e.g. Esc, Backspace, Left, Right).
    Code(event::KeyCode),
    /// A key modification with Control pressed (e.g. Ctrl+n, Ctrl+p).
    Ctrl(char),
    /// A key modification with Shift pressed (e.g. Shift+Esc).
    Shift(event::KeyCode),
    /// Mouse wheel scroll up action.
    ScrollUp,
    /// Mouse wheel scroll down action.
    ScrollDown,
}

impl KeyDef {
    /// Evaluates if an incoming crossterm Event matches the configured KeyDef shortcut.
    pub fn matches(self, event: &event::Event) -> bool {
        match event {
            event::Event::Key(key_event) if key_event.kind == event::KeyEventKind::Press => {
                use event::{KeyCode, KeyModifiers};
                match self {
                    Self::Char(c) => {
                        if let KeyCode::Char(key_char) = key_event.code {
                            if key_event.modifiers.contains(KeyModifiers::CONTROL)
                                || key_event.modifiers.contains(KeyModifiers::ALT)
                            {
                                return false;
                            }
                            if c.is_alphabetic() {
                                if c.is_lowercase() {
                                    key_char == c
                                        && !key_event.modifiers.contains(KeyModifiers::SHIFT)
                                } else {
                                    key_char == c
                                        || (key_event.modifiers.contains(KeyModifiers::SHIFT)
                                            && key_char.to_lowercase().next()
                                                == c.to_lowercase().next())
                                }
                            } else {
                                key_char == c
                            }
                        } else {
                            false
                        }
                    }
                    Self::Code(code) => {
                        key_event.code == code
                            && !key_event.modifiers.contains(KeyModifiers::CONTROL)
                            && !key_event.modifiers.contains(KeyModifiers::SHIFT)
                            && !key_event.modifiers.contains(KeyModifiers::ALT)
                    }
                    Self::Ctrl(c) => {
                        if let KeyCode::Char(key_char) = key_event.code {
                            key_char == c
                                && key_event.modifiers.contains(KeyModifiers::CONTROL)
                                && !key_event.modifiers.contains(KeyModifiers::ALT)
                        } else {
                            false
                        }
                    }
                    Self::Shift(code) => {
                        key_event.code == code
                            && key_event.modifiers.contains(KeyModifiers::SHIFT)
                            && !key_event.modifiers.contains(KeyModifiers::CONTROL)
                            && !key_event.modifiers.contains(KeyModifiers::ALT)
                    }
                    Self::ScrollUp | Self::ScrollDown => false,
                }
            }
            event::Event::Mouse(mouse_event) => match self {
                Self::ScrollUp => matches!(mouse_event.kind, event::MouseEventKind::ScrollUp),
                Self::ScrollDown => matches!(mouse_event.kind, event::MouseEventKind::ScrollDown),
                _ => false,
            },
            _ => false,
        }
    }
}

impl std::fmt::Display for KeyDef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Char(c) => match c {
                ' ' => write!(f, "Space"),
                _ => write!(f, "{}", c),
            },
            Self::Code(code) => match code {
                event::KeyCode::Esc => write!(f, "Esc"),
                event::KeyCode::Backspace => write!(f, "Backspace"),
                event::KeyCode::Left => write!(f, "Left"),
                event::KeyCode::Right => write!(f, "Right"),
                event::KeyCode::Up => write!(f, "Up"),
                event::KeyCode::Down => write!(f, "Down"),
                event::KeyCode::Char(' ') => write!(f, "Space"),
                event::KeyCode::F(n) => write!(f, "F{}", n),
                _ => write!(f, "Other"),
            },
            Self::Ctrl(c) => write!(f, "Ctrl+{}", c),
            Self::Shift(code) => match code {
                event::KeyCode::Esc => write!(f, "Shift+Esc"),
                event::KeyCode::Backspace => write!(f, "Shift+Backspace"),
                event::KeyCode::Left => write!(f, "Shift+Left"),
                event::KeyCode::Right => write!(f, "Shift+Right"),
                event::KeyCode::Up => write!(f, "Shift+Up"),
                event::KeyCode::Down => write!(f, "Shift+Down"),
                _ => write!(f, "Shift+?"),
            },
            Self::ScrollUp => write!(f, "Scroll Up"),
            Self::ScrollDown => write!(f, "Scroll Down"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    #[test]
    fn test_key_modifier_matching() {
        let press_right = Event::Key(KeyEvent {
            code: KeyCode::Right,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });

        let press_shift_right = Event::Key(KeyEvent {
            code: KeyCode::Right,
            modifiers: KeyModifiers::SHIFT,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });

        let press_ctrl_right = Event::Key(KeyEvent {
            code: KeyCode::Right,
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });

        let key_code_right = KeyDef::Code(KeyCode::Right);
        let key_shift_right = KeyDef::Shift(KeyCode::Right);

        // Code(Right) should only match exact Right arrow, not Shift+Right or Ctrl+Right
        assert!(key_code_right.matches(&press_right));
        assert!(!key_code_right.matches(&press_shift_right));
        assert!(!key_code_right.matches(&press_ctrl_right));

        // Shift(Right) should match Shift+Right, not exact Right
        assert!(key_shift_right.matches(&press_shift_right));
        assert!(!key_shift_right.matches(&press_right));
    }
}
