---
name: line-editing
description: Interactive input delegation, UTF-8 safety, LineEditor widget logic, and input validations.
---

# Interactive Input & UTF-8 Safety Invariants

### 1. Interactive Line Editing

- Delegate interactive command/query line editing to a specialized `LineEditor` widget rather than processing keypresses directly in the app state controller.
- Event handlers must map `KeyEvent` updates into a matching `EditorResult` enum to control downstream rendering updates:
  - `ConsumedChanged`: Text content updated. Re-calculate matching candidates list and reset selection index.
  - `ConsumedNoChange`: Text cursor/viewport moved without text modification. Trigger immediate frame redraw without filtering updates.
  - `NotConsumed`: Key was not captured by the editor (e.g. `Enter` or `Esc`) and must be bubbled up to parent controls.

### 2. UTF-8 Slice Safety

- **Invariant**: Never perform raw byte slicing on strings (e.g. `&path[..offset]`). Emojis, Nerdfont glyphs, and non-ASCII Unix path characters occupy variable byte counts; raw slicing inside multi-byte boundaries causes runtime boundary panic.
- Always use `.char_indices()` to isolate UTF-8 character boundaries:
  ```rust
  let char_indices: Vec<(usize, char)> = s.char_indices().collect();
  if char_indices.len() > max_chars {
      let boundary_byte_idx = char_indices[char_indices.len() - max_chars].0;
      format!("...{}", &s[boundary_byte_idx..])
  } else {
      s.to_string()
  }
  ```
