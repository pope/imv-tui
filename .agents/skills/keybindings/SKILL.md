---
name: keybindings
description: Keybindings mapping schemas, CLI argument configuration, and command metadata rules.
---

# Unified Keybindings & Command Architecture

### 1. Unified Event Matching

- **Multi-event mappings**: Match shortcuts against `crossterm::event::Event` rather than just `KeyEvent` to support unified handling of keyboard strokes, mouse wheel scrolls, and complex combinations.
- **Strict Modifier Checks**: To prevent shortcut collisions, all keys matching must explicitly exclude irrelevant modifiers:
  - `KeyDef::Char(c)` must verify CONTROL and ALT are absent.
  - `KeyDef::Code(code)` must verify CONTROL, SHIFT, and ALT are absent.
  - `KeyDef::Shift(code)` must verify CONTROL and ALT are absent.

### 2. Declarative Command Metadata

- **1-to-1 Mapping**: Maintain a single source of truth for command configuration. Map every variant of the `Command` enum to a metadata struct returning:
  - User-facing description.
  - Display shortcut representation (e.g. `Ctrl+h`).
  - Search visibility flag in the Command Palette.
- **Pre-calculated Formats**: Pre-calculate static formatted strings (like palette display rows) during application startup or registry initialization instead of joining/collecting arrays inside hot rendering loops.
