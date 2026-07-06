---
name: tui-architecture
description: Guidelines on RAII safety guards, pure rendering viewports, command-to-shortcut maps, and modular separations in Rust TUI applications.
---

# Rust TUI Architectural Guidelines

### 1. RAII Terminal Guards

- **Alt-Screen Exit Invariant**: Standard panic crashes or returns (`?`) can exit the binary before Crossterm disables raw mode and alternate screen, leaving the terminal state corrupted.
- **TerminalGuard Widget**: Implement a RAII `TerminalGuard` with a `Drop` implementation that restores raw mode and screen settings.
- **Drop Order**: Drop the guard explicitly *before* printing any stdout data intended for pipe composition on program exit, ensuring the stdout output remains visible in the parent shell history.

### 2. Pure Rendering Viewports

- **Zero Mutations**: Drawing view functions (under `src/ui/`) must be read-only and side-effect free. Pass immutable references `app: &App` (rather than `&mut App`).
- **Pre-calculated Layouts**: Calculate all sizes, viewport coordinates, and overlays in a pre-calculation phase in the state controller (`App::update_layout()`) *before* invoking `terminal.draw`.
- **Defensive Clamping**: Defensively clamp calculated popup coordinate parameters (`rect_x`, `rect_y`, `rect_w`, `rect_h`) and cursor offsets to fit within the viewport area.
- **Delegated Viewports**: Keep `src/ui/mod.rs` clean (\<100 lines) by delegating all view blocks and dialog layouts (prompts, command palettes, stats lists) into individual submodules under `src/ui/views/`.

### 3. Modular File Encapsulation

- Keep file responsibilities clean and modularized:
  - `src/main.rs`: Alt-screen, raw mode, panic hooks, and main event loops.
  - `src/config/`: CLI parsing (`cli.rs`) and raw key definitions (`keys.rs`).
  - `src/commands/`: Key mappings and command registry metadata.
  - `src/imaging/`: Decoders, directory scanning, and imaging clamping types.
  - `src/app/`: Controller state, adjustments db, and background worker handles.
  - `src/ui/`: Pure layout templates, dialog overlays, HUD widgets.
