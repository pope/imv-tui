---
name: tui-architecture
description: Guidelines on RAII safety guards, pure rendering viewports, command-to-shortcut maps, and modular separations in Rust TUI applications.
---

# Rust TUI Architectural Guidelines

### 1. RAII Terminal Guards & Clean Panics
When building TUI applications, standard crash panics or early returns (`?`) can exit the binary before Crossterm has restored raw terminal settings and left the alternate screen.
* **Terminal Guard**: Implement a RAII `TerminalGuard` that triggers standard terminal restoration inside its `Drop` implementation:
  ```rust
  struct TerminalGuard;
  impl Drop for TerminalGuard {
      fn drop(&mut self) {
          let _ = disable_raw_mode();
          let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
      }
  }
  ```
* **Custom Panic Hooks**: Register a custom panic hook at startup to clean up alternate screen and raw modes before writing panic details to stderr.

### 2. Pure Rendering Viewports & separation of concerns
Mutating application states (such as layout dimensions or screen-clearing triggers) inside the drawing loops of `src/ui.rs` breaks the read-only rendering guarantee, leads to visual lag, and triggers double terminal refreshes.
* **Layout Update Phase**: Introduce a layout pre-calculation phase in the controller (`App::update_layout(term_height)`) to update sizes, check boundaries, and trigger clear signals *before* invoking `terminal.draw`.
* **Pure Rendering**: Enforce read-only rendering logic inside `src/ui.rs` where layout parameters are purely read from precomputed state variables.
* **Rect Boundary Clamping**: Always clamp calculated coordinates (`rect_w`, `rect_h`) against viewport dimensions to prevent crashes and coordinate overflows during terminal resizing.

### 3. Compiler-Enforced Unified Keybindings & Command Architecture
* **1-to-1 Command Mappings**: Map each variant of the `Command` enum to a single metadata `CommandItem` struct. This centralizes description name, palette search visibility, and keyboard shortcuts configuration in one location (`get_metadata()`).
* **General Event Matching**: Match shortcuts against `crossterm::event::Event` rather than just `KeyEvent` to support unified mapping of keyboard keys, mouse scroll updates, and custom combinations.
* **Pre-computed UI Formats**: Pre-calculate static formatted strings (like command shortcuts) once at initialization instead of joining/collecting arrays inside hot rendering loops.
* **Declarative Expressions**: Prefer iterator chains (`find`, `any`, `filter`, `cloned`, `map`) over imperative search blocks to make code more readable and idiomatic.

### 4. Modular Encapsulation & File Separation
Keep file responsibilities clean and modularized:
* `src/main.rs`: Entry point, raw-mode initialization, panic hooks, and Crossterm event loop.
* `src/cli.rs`: CLI command line option parsing, argument validation, and stdin path piping.
* `src/commands.rs`: Mappings of keys, keyboard descriptions, command metadata, and shortcuts.
* `src/image_worker.rs`: Image source decoders, directory scans, and background resizers.
* `src/app.rs`: State controller and worker thread management.
* `src/ui.rs`: Layout and widget rendering views.
