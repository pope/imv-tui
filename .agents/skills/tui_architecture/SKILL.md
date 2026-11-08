---
name: tui-architecture
description: Guidelines on RAII safety guards, pure rendering viewports via UiModel, 3-phase game loop, and strict DAG boundaries in Rust TUI applications.
---

# Rust TUI Architectural Guidelines

### 1. RAII Terminal Guards

- **Alt-Screen Exit Invariant**: Standard panic crashes or early returns (`?`) can exit the binary before Crossterm disables raw mode and alternate screen, leaving the terminal state corrupted.
- **TerminalGuard Widget**: Implement a RAII `TerminalGuard` with a `Drop` implementation that restores raw mode, leaves the alternate screen, and re-shows the cursor.
- **Drop Order**: Drop the guard explicitly *before* printing any stdout data intended for pipe composition on program exit, ensuring the stdout output remains visible in the parent shell history.

### 2. Pure Rendering Viewports & `UiModel`

- **Zero Mutations & Disjoint View Models**: Drawing view functions (under `src/ui/`) must be read-only and side-effect free, having **zero** direct dependencies on `App`.
- **`UiModel<'a>` Presentation Contract**: All layout shapes, line editor states, infobar metrics, active protocol widgets, and overlays are assembled in a single snapshot `UiModel<'a>` via `App::ui_model(&mut self)`. The rendering entrypoint is strictly:
  ```rust
  terminal.draw(|f| {
      let mut model = app.ui_model();
      ui(f, &mut model);
  })?;
  ```
- **Pre-calculated Layouts**: Calculate all sizes, viewport coordinates, and widget geometry in the state controller (`App::update_layout()`) *before* invoking `terminal.draw`.
- **Defensive Clamping**: Defensively clamp calculated popup coordinate parameters (`rect_x`, `rect_y`, `rect_w`, `rect_h`) and cursor offsets to fit within the viewport area.
- **Delegated Viewports**: Keep `src/ui/mod.rs` clean (\<100 lines) by delegating all view blocks and dialog layouts (prompts, command palettes, stats lists) into individual submodules under `src/ui/views/`.

### 3. Strict DAG & 3-Phase Frame Loop

- **Strict One-Way Dependency DAG**:

  ```text
  src/config/ ──┐
  src/imaging/ ─┼─> src/ui/ ──> src/app/ ──> src/main.rs
  src/commands/ ┘
  ```

  - Lower modules (`src/config/`, `src/imaging/`, `src/commands/`, `src/ui/`) must have **zero** imports from `crate::app`:
    `! git grep "crate::app" src/config/ src/imaging/ src/commands/ src/ui/`.

- **Module Responsibilities**:

  - `src/main.rs`: Alt-screen, raw mode, panic hooks, and the 3-phase event loop.
  - `src/config/`: CLI parsing (`cli.rs`), key definitions (`keys.rs`), position enums (`InfoBarPosition`), `Classification`, and `ViewMode`.
  - `src/imaging/`: Decoders, directory scanning, format detection, SIMD pixel adjusters, and domain clamping types (`Brightness`, `Contrast`, `PanOffset`, `CropBox`).
  - `src/commands/`: Pure `InputAction`, `OverlayAction`, `EditorAction`, `LineEditor`, `translate_event`, and command registry metadata.
  - `src/ui/`: Presentation models (`UiModel`, `ViewportModel`, `InfoBarModel`, `InfoBoxModel`, `PaletteViewModel`), pure layout templates, dialog overlays, and HUD widgets.
  - `src/app/`: Central controller state machine (`ResizeState`, `ScreenClear`, `InitialLoadState`, `ScrollCadence`), action reducer (`apply_action`), and background worker handles.

- **3-Phase Game-Loop Architecture (`peng`-style)**:
  Every iteration of the main loop runs in three deterministic, sequential phases:

  1. **Phase 1: Input Translation & Reduction**:
     - Poll and drain events into a coalesced buffer.
     - Translate raw Crossterm events into pure `InputAction` values using `translate_event(palette_mode, &ev)`.
     - Reduce actions into application state via `app.apply_action(action, term_size.height)`.
  2. **Phase 2: Systems & Background Worker Ticks**:
     - Advance timers and system clocks (`app.tick_slideshow()`).
     - Drain non-blocking worker channels for loader and resizer responses (`app.update_channels()`).
     - Re-evaluate settled cadence and clear flags (`app.tick()`).
     - Update layout dimensions for viewport and protocol sizing (`app.update_layout()`).
  3. **Phase 3: Presentation & Drawing**:
     - If a redraw is flagged (`draw_needed`), project the app state into a pure `UiModel`:
       ```rust
       terminal.draw(|f| {
           let mut model = app.ui_model();
           ui(f, &mut model);
       })?;
       ```
