use crossterm::event;

/// Abstract representation of keyboard keys and mouse actions for bindings.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyDef {
    /// A single character press (e.g. 'q', 'i').
    Char(char),
    Code(event::KeyCode),
    Ctrl(char),
    Shift(event::KeyCode),
    ScrollUp,
    ScrollDown,
}

impl KeyDef {
    pub fn matches(self, event: &event::Event) -> bool {
        match event {
            event::Event::Key(key_event) if key_event.kind == event::KeyEventKind::Press => {
                use event::{KeyCode, KeyModifiers};
                match self {
                    Self::Char(c) => {
                        if let KeyCode::Char(key_char) = key_event.code {
                            if key_event.modifiers.contains(KeyModifiers::CONTROL) {
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
                    }
                    Self::Ctrl(c) => {
                        if let KeyCode::Char(key_char) = key_event.code {
                            key_char == c && key_event.modifiers.contains(KeyModifiers::CONTROL)
                        } else {
                            false
                        }
                    }
                    Self::Shift(code) => {
                        key_event.code == code && key_event.modifiers.contains(KeyModifiers::SHIFT)
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
