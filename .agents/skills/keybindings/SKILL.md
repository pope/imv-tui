---
name: keybindings
description: Keybindings mapping schemas, InputAction translation, CLI argument configuration, and command metadata rules.
---

# Unified Keybindings & Command Architecture

### 1. Pure `InputAction` & Two-Tier Event Translation

- **Pure Translation Function**: Raw Crossterm events (`crossterm::event::Event`) are translated into pure `InputAction` values via `translate_event(palette_mode, &Event) -> Option<InputAction>` located in `src/commands/actions.rs`.
- **Pure Action Hierarchy**:
  - `InputAction::Command(Command)`: Normal-mode global application commands (zoom, pan, rotate, navigate, toggle HUD, exit).
  - `InputAction::Editor(EditorAction)`: Text editing and cursor movement inside an active interactive input overlay (`Command`, `File`, `Prompt`), delegated to `LineEditor`.
  - `InputAction::Overlay(OverlayAction)`: Modal overlay navigation, confirmation, and dismissal:
    - `Close`: Close active overlay (`Esc`, or `q` when in `PaletteMode::Info`).
    - `Confirm`: Confirm dialog or execute selected palette item (`Enter`).
    - `SelectPrev`: Move palette selection up by one row (`Up`, `Ctrl+P`, or mouse scroll up).
    - `SelectNext`: Move palette selection down by one row (`Down`, `Ctrl+N`, `Ctrl+J`, or mouse scroll down).
    - `SelectPageUp`: Move palette selection up by one page (`PageUp` or `Alt+V`).
    - `SelectPageDown`: Move palette selection down by one page (`PageDown` or `Ctrl+V`).
- **Decoupled Architecture**: `src/commands/` has **zero** dependencies on `src/app/`. The state controller in `src/app/` strictly acts as a reducer via `App::apply_action(action, terminal_height)`.

### 2. High-Speed $O(1)$ Command Lookup & Modifier Matching

- **Two-Tier Command Matching (`src/commands/mod.rs`)**:
  - **Tier 1 (ASCII Fast-Path)**: An `Option<Command>` array table sized for ASCII characters (`[Option<Command>; 128]`) performs $O(1)$ lookup for unadorned alphanumeric shortcuts without hashing overhead.
  - **Tier 2 (Fallback Map)**: A `HashMap<KeyDef, Command>` handles modified keys, non-ASCII codepoints, arrow keys, and mouse scroll events.
- **Strict Modifier Checks**: To prevent shortcut collisions, all keys matching must explicitly exclude irrelevant modifiers:
  - `KeyDef::Char(c)` must verify CONTROL and ALT are absent.
  - `KeyDef::Code(code)` must verify CONTROL, SHIFT, and ALT are absent.
  - `KeyDef::Shift(code)` must verify CONTROL and ALT are absent.

### 3. Declarative Command Metadata

- **1-to-1 Mapping**: Maintain a single source of truth for command configuration. Map every variant of the `Command` enum to a metadata struct returning:
  - User-facing description.
  - Display shortcut representation (e.g. `Ctrl+h`).
  - Search visibility flag in the Command Palette.
- **Pre-calculated Formats**: Pre-calculate static formatted strings (like palette display rows) during application startup or registry initialization instead of joining/collecting arrays inside hot rendering loops.
