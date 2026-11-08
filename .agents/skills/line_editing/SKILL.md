---
name: line-editing
description: Interactive input delegation, EditorAction translation, zero-allocation UTF-8 safety, LineEditor widget logic, and input validations.
---

# Interactive Input & UTF-8 Safety Invariants

### 1. Interactive Line Editing & `EditorAction`

- Delegate interactive command/query line editing to a self-contained `LineEditor` located in `src/commands/line_editor.rs` with **zero** `crate::app` dependencies.
- Event translation maps raw `KeyEvent` values into a pure `EditorAction` enum:
  ```rust
  let action = EditorAction::from_key_event(key_event);
  ```
- The pure `EditorAction` encompasses:
  - `MoveLeft`: Move cursor one character to the left (`Left` or `Ctrl+B`).
  - `MoveRight`: Move cursor one character to the right (`Right` or `Ctrl+F`).
  - `MoveToStart`: Move cursor to the start of the line (`Home` or `Ctrl+A`).
  - `MoveToEnd`: Move cursor to the end of the line (`End` or `Ctrl+E`).
  - `DeleteToEnd`: Delete from cursor to the end of the line (`Ctrl+K`).
  - `DeleteToStart`: Delete from cursor to the start of the line (`Ctrl+U`).
  - `DeleteWordBefore`: Delete the word preceding the cursor (`Ctrl+W` or `Alt+Backspace`).
  - `DeleteChar`: Delete the character under the cursor (`Delete` or `Ctrl+D`).
  - `Backspace`: Delete the character before the cursor (`Backspace`).
  - `InsertChar(char)`: Insert a literal character at the cursor position.
- `LineEditor::apply_action(&mut self, action: EditorAction) -> bool` executes the editing action:
  - Returns `true` when buffer content changed, triggering `update_palette_filter()` and resetting `palette_selected_index = 0`.
  - Returns `false` on cursor-only moves or no-ops, preserving the active filtered list and current selection.

### 2. Zero-Allocation UTF-8 Slice Safety

- **Invariant**: Never perform raw byte slicing on strings (e.g. `&path[..offset]`). Emojis, Nerdfont glyphs, and non-ASCII Unix path characters occupy variable byte counts; raw slicing inside multi-byte boundaries causes runtime boundary panics.
- **Zero-Allocation Boundary Discovery**: Never collect characters into a temporary `Vec<(usize, char)>` on the heap. Use zero-allocation reverse iterator inspection on `.char_indices()`:
  ```rust
  fn get_elided_suffix(s: &str, elided_len: usize) -> std::borrow::Cow<'_, str> {
      if elided_len == 0 {
          return std::borrow::Cow::Borrowed("...");
      }
      if s.char_indices().rev().nth(elided_len).is_some() {
          let (start_idx, _) = s.char_indices().rev().nth(elided_len - 1).unwrap();
          std::borrow::Cow::Owned(format!("...{}", &s[start_idx..]))
      } else {
          std::borrow::Cow::Borrowed(s)
      }
  }
  ```
- **Cursor Positioning**: Always compute rendered visual column widths using `ratatui::text::Span::raw(&query[..cursor_byte_offset]).width() as u16` rather than byte counts or raw character counts, ensuring full compatibility with multi-byte or double-width CJK/emoji glyphs.
