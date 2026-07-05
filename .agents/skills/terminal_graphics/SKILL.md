---
name: terminal-graphics
description: Guidelines and learnings for foot/sixel/kitty terminal graphic rendering, selective visual clearing, and input repeats coalescing in imv-tui.
---

# Terminal Graphics & Redrawing (Sixel vs. Kitty)

### 1. Protocols & Double Buffering

Terminal image rendering libraries (like `ratatui-image`) display graphics using custom escape protocols:

- **Kitty** uses referenceable overlay buffers that can be selectively redrawn or removed by the terminal.
- **Sixel** writes graphics directly onto the text grid cells of the terminal screen.
- **Double Buffering Conflict**: `ratatui` uses a double-buffering algorithm to only draw terminal cell changes. When text overlay popups (like the Help menu) are dismissed, the terminal double-buffer may not detect a cell change because Sixel widgets do not output standard space character bytes. This leaves the old text overlay characters frozen on top of the image layer in Sixel terminals (such as Foot).
- **Text Overlay Graphics Erasure Bug (Clear to End of Line)**: When drawing text overlays centered on top of active terminal graphics (Kitty or Sixel overlays), the TUI backend (like `ratatui`'s `CrosstermBackend` using `\x1b[K`) optimizes rendering by erasing characters to the right of the print cursor. In Kitty, this erases the graphic overlay from the cursor position to the right edge of the screen.

### 2. Selective Clearing (Anti-Flicker)

- Do not clear the entire terminal buffer on continuous updates (like zooming or panning) as it causes annoying screen flicker.
- When toggling overlay panels (e.g. Help menus) or swapping files/rotations, trigger a centralized `needs_clear` flag.
- Conditionally call `terminal.clear()?` *only* if the active protocol requires it. Check the active protocol using the `Picker`:
  ```rust
  pub fn should_clear_on_update(&self) -> bool {
      matches!(self.picker.protocol_type(), ratatui_image::picker::ProtocolType::Sixel)
  }
  ```
- Defer screen clearing until the new protocol is actually received from the worker thread.
- **Synchronize Protocol and Cell Layout updates**: Pass target cell layout dimensions `rendered_size_cells` along with the `ResizeRequest` to the background thread. Apply both at the exact same instant inside the channel receiver.

### 3. Text Overlays on Top of Graphics

- **Image Redraw covers Text**: Triggering `update_protocol()` while a text dialog is open causes the graphic layer to cover the text grid. Because of `ratatui`'s double buffering, only the text cells that *changed* will be redrawn on top of the image.
- **Erase Text in Kitty/WezTerm**: Dismissing/closing a text dialog does not automatically clear the characters (spaces) in double buffered Kitty-protocol terminals. To cleanly wipe these character cells, set an unconditional `needs_clear_once = true` flag when closing an overlay.
- **No Updates on Typing/Navigation**: Do not set `needs_update = true` or call `update_protocol()` for keystrokes, character entry, or row selection inside dialog inputs.
- **No Unnecessary Resizes on State/Toggle Events**: Toggling states (like pausing/playing slideshows) must not set `needs_update = true` or trigger `update_protocol()` unless layout dimensions, active filters, zoom, or contrast values have actually changed. Resizing causes a new graphic payload to transmit, which draws over/hides the overlay dialog on Kitty terminals.

### 4. Input Event Coalescing/Debouncing (Keyboard Repeats)

Before drawing a frame, block for the first key event, then immediately drain all remaining pending key repeat events from Crossterm's event buffer using `poll(0)`:

```rust
if event::poll(Duration::from_millis(50))? {
    let mut events = Vec::new();
    events.push(event::read()?);
    while event::poll(Duration::from_millis(0))? {
        events.push(event::read()?);
    }
    // Process all events and update offsets before running update_protocol().
}
```

- **TUI Redraw Feedback Loop Prevention**: Terminal emulators (like Kitty or Sixel-capable ones) write response/query sequences back to stdin when rendering graphics. To avoid triggering infinite drawing feedback loops and high CPU usage, only set `draw_needed = true` inside the event loop on meaningful user interactions (`Event::Key`, `Event::Mouse`, `Event::Paste`) or screen changes (`Event::Resize`), filtering out protocol responses.
