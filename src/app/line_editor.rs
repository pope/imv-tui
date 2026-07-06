use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// The outcome of the LineEditor handling a key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorResult {
    /// The key was not handled by the editor (e.g., navigation, submit, cancel).
    NotConsumed,
    /// The key was handled, but the input value did not change (e.g., cursor movement).
    ConsumedNoChange,
    /// The key was handled, and the input value changed.
    ConsumedChanged,
}

/// A simple, cursor-aware line editing buffer.
#[derive(Debug, Clone, Default)]
pub struct LineEditor {
    value: String,
    cursor_char_idx: usize,
}

impl LineEditor {
    /// Handles editing or cursor navigation key events.
    pub fn handle_key_event(&mut self, key: &KeyEvent) -> EditorResult {
        match key.code {
            // Left / Ctrl+B
            KeyCode::Left | KeyCode::Char('b')
                if key.code == KeyCode::Left || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.move_left();
                EditorResult::ConsumedNoChange
            }
            // Right / Ctrl+F
            KeyCode::Right | KeyCode::Char('f')
                if key.code == KeyCode::Right || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.move_right();
                EditorResult::ConsumedNoChange
            }
            // Home / Ctrl+A
            KeyCode::Home | KeyCode::Char('a')
                if key.code == KeyCode::Home || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.move_to_start();
                EditorResult::ConsumedNoChange
            }
            // End / Ctrl+E
            KeyCode::End | KeyCode::Char('e')
                if key.code == KeyCode::End || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.move_to_end();
                EditorResult::ConsumedNoChange
            }
            // Ctrl+K (delete to end)
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.delete_to_end() {
                    EditorResult::ConsumedChanged
                } else {
                    EditorResult::ConsumedNoChange
                }
            }
            // Ctrl+U (delete to start)
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.delete_to_start() {
                    EditorResult::ConsumedChanged
                } else {
                    EditorResult::ConsumedNoChange
                }
            }
            // Ctrl+W (delete word before)
            KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.delete_word_before() {
                    EditorResult::ConsumedChanged
                } else {
                    EditorResult::ConsumedNoChange
                }
            }
            // Ctrl+D / Delete
            KeyCode::Char('d') | KeyCode::Delete
                if key.code == KeyCode::Delete || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                if self.delete() {
                    EditorResult::ConsumedChanged
                } else {
                    EditorResult::ConsumedNoChange
                }
            }
            // Alt+Backspace (delete word before)
            KeyCode::Backspace if key.modifiers.contains(KeyModifiers::ALT) => {
                if self.delete_word_before() {
                    EditorResult::ConsumedChanged
                } else {
                    EditorResult::ConsumedNoChange
                }
            }
            // Backspace (delete char before)
            KeyCode::Backspace => {
                if self.backspace() {
                    EditorResult::ConsumedChanged
                } else {
                    EditorResult::ConsumedNoChange
                }
            }
            // Literal characters
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.insert_char(c);
                EditorResult::ConsumedChanged
            }
            _ => EditorResult::NotConsumed,
        }
    }

    pub fn new() -> Self {
        Self {
            value: String::new(),
            cursor_char_idx: 0,
        }
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    #[allow(dead_code)]
    pub fn cursor_char_idx(&self) -> usize {
        self.cursor_char_idx
    }

    pub fn clear(&mut self) {
        self.value.clear();
        self.cursor_char_idx = 0;
    }

    pub fn cursor_byte_offset(&self) -> usize {
        self.byte_offset_for_char_idx(self.cursor_char_idx)
            .unwrap_or(self.value.len())
    }

    fn byte_offset_for_char_idx(&self, idx: usize) -> Option<usize> {
        self.value.char_indices().map(|(i, _)| i).nth(idx)
    }

    /// Inserts a character at the current cursor position.
    pub fn insert_char(&mut self, c: char) {
        let byte_offset = self.cursor_byte_offset();
        self.value.insert(byte_offset, c);
        self.cursor_char_idx += 1;
    }

    /// Deletes the character before the current cursor position.
    pub fn backspace(&mut self) -> bool {
        if self.cursor_char_idx == 0 {
            return false;
        }
        let char_idx = self.cursor_char_idx - 1;
        if let Some(offset) = self.byte_offset_for_char_idx(char_idx) {
            self.value.remove(offset);
            self.cursor_char_idx = char_idx;
            true
        } else {
            false
        }
    }

    /// Deletes the character at the current cursor position (Delete / Ctrl+D).
    pub fn delete(&mut self) -> bool {
        if let Some(offset) = self.byte_offset_for_char_idx(self.cursor_char_idx) {
            self.value.remove(offset);
            true
        } else {
            false
        }
    }

    pub fn move_left(&mut self) {
        self.cursor_char_idx = self.cursor_char_idx.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        let total_chars = self.value.chars().count();
        if self.cursor_char_idx < total_chars {
            self.cursor_char_idx += 1;
        }
    }

    pub fn move_to_start(&mut self) {
        self.cursor_char_idx = 0;
    }

    pub fn move_to_end(&mut self) {
        self.cursor_char_idx = self.value.chars().count();
    }

    /// Deletes from the cursor to the end of the line (Ctrl+K).
    pub fn delete_to_end(&mut self) -> bool {
        let byte_offset = self.cursor_byte_offset();
        if byte_offset < self.value.len() {
            self.value.truncate(byte_offset);
            true
        } else {
            false
        }
    }

    /// Deletes from the cursor to the beginning of the line (Ctrl+U).
    pub fn delete_to_start(&mut self) -> bool {
        if self.cursor_char_idx == 0 {
            return false;
        }
        let byte_offset = self.cursor_byte_offset();
        self.value.drain(0..byte_offset);
        self.cursor_char_idx = 0;
        true
    }

    /// Deletes the word before the current cursor position (Ctrl+W).
    pub fn delete_word_before(&mut self) -> bool {
        if self.cursor_char_idx == 0 {
            return false;
        }

        // Find the cursor byte offset
        let cursor_offset = self.cursor_byte_offset();

        // Slice before the cursor
        let text_before = &self.value[..cursor_offset];

        // Find the start of the word to delete
        // Standard behavior: skip trailing whitespace, then delete up to next whitespace
        let mut chars = text_before.char_indices().rev().peekable();

        // Skip trailing whitespace
        while let Some((_, c)) = chars.peek() {
            if c.is_whitespace() {
                chars.next();
            } else {
                break;
            }
        }

        // Read non-whitespace word characters
        while let Some((_, c)) = chars.peek() {
            if !c.is_whitespace() {
                chars.next();
            } else {
                break;
            }
        }

        let new_offset = if let Some((idx, c)) = chars.next() {
            idx + c.len_utf8()
        } else {
            0
        };

        if new_offset < cursor_offset {
            self.value.replace_range(new_offset..cursor_offset, "");
            self.cursor_char_idx = self.value[..new_offset].chars().count();
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_line_editor_basic() {
        let mut le = LineEditor::new();
        assert_eq!(le.value(), "");
        assert_eq!(le.cursor_char_idx(), 0);

        le.insert_char('a');
        le.insert_char('b');
        assert_eq!(le.value(), "ab");
        assert_eq!(le.cursor_char_idx(), 2);

        le.move_left();
        assert_eq!(le.cursor_char_idx(), 1);

        le.insert_char('c');
        assert_eq!(le.value(), "acb");
        assert_eq!(le.cursor_char_idx(), 2);

        assert!(le.backspace());
        assert_eq!(le.value(), "ab");
        assert_eq!(le.cursor_char_idx(), 1);

        assert!(le.delete());
        assert_eq!(le.value(), "a");
        assert_eq!(le.cursor_char_idx(), 1);
    }

    #[test]
    fn test_line_editor_movements() {
        let mut le = LineEditor::new();
        le.insert_char('x');
        le.insert_char('y');
        le.insert_char('z');

        le.move_to_start();
        assert_eq!(le.cursor_char_idx(), 0);

        le.move_right();
        assert_eq!(le.cursor_char_idx(), 1);

        le.move_to_end();
        assert_eq!(le.cursor_char_idx(), 3);
    }

    #[test]
    fn test_line_editor_readline_deletions() {
        let mut le = LineEditor::new();
        le.insert_char('a');
        le.insert_char('b');
        le.insert_char('c');
        le.insert_char('d');

        le.move_left(); // cursor at 3 (before 'd')
        assert!(le.delete_to_end());
        assert_eq!(le.value(), "abc");
        assert_eq!(le.cursor_char_idx(), 3);

        le.move_left(); // cursor at 2 (before 'c')
        assert!(le.delete_to_start());
        assert_eq!(le.value(), "c");
        assert_eq!(le.cursor_char_idx(), 0);
    }

    #[test]
    fn test_line_editor_delete_word() {
        let mut le = LineEditor::new();
        le.insert_char('h');
        le.insert_char('e');
        le.insert_char('l');
        le.insert_char('l');
        le.insert_char('o');
        le.insert_char(' ');
        le.insert_char('w');
        le.insert_char('o');
        le.insert_char('r');
        le.insert_char('l');
        le.insert_char('d');

        assert!(le.delete_word_before());
        assert_eq!(le.value(), "hello ");

        assert!(le.delete_word_before());
        assert_eq!(le.value(), "");
        assert_eq!(le.cursor_char_idx(), 0);
    }
}
