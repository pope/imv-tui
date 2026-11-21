---
name: terminal-graphics
description: Guidelines and learnings for foot/sixel/kitty terminal graphic rendering, selective visual clearing, and input repeats coalescing in imv-tui.
---

# Terminal Graphics & Redrawing (Sixel vs. Kitty)

### 1. Rendering Protocols & Overlay Conflicts

- **Kitty vs Sixel**: Kitty overlays graphics onto a dedicated buffer; Sixel renders direct graphics over text cells.
- **Double Buffering Bug**: Space characters in text overlays (e.g. Help menu) will freeze on top of Sixel layers upon dismissal. Erasing text lines using cursor operations (like `\x1b[K`) will wipe overlapping Kitty graphics.
- **Redraw Cover Invariant**: Triggering any graphic resize/refresh while an overlay dialog is active will draw the image *over* the text. Do not call `mark_dirty()` on `ResizeState` or dispatch resize requests for text inputs, menu navigation, or state changes (like pausing slideshows).
- **Type-Safe Resize Pipeline**: Viewport protocol resizing is governed by `ResizeState`:
  - `ResizeState::Idle`: Protocol matches current widget dimensions.
  - `ResizeState::Dirty { init_zoom }`: Viewport size changed or zoom requires re-initialization. Worker request must be dispatched.
  - `ResizeState::InFlight`: Resizing worker is processing in the background. Subsequent resize events update dirty targets without duplicating requests.

### 2. Selective Clearing (`ScreenClear` State Machine)

- **Selective Clears**: Never clear the screen globally on continuous updates (zoom/pan) to avoid terminal flicker.
- **Centralized `ScreenClear` State Machine**: Clearing state is managed explicitly via `ScreenClear`:
  - `ScreenClear::None`: No clear required.
  - `ScreenClear::OnProtocolReceive`: Scheduled clear deferred until the next graphic protocol response arrives from the worker, avoiding blank flashes.
  - `ScreenClear::Pending`: Immediate clear required before drawing (e.g. upon closing dialog overlays or rotating images).
- **Protocol-Gated Execution**: Screen clearing is executed on Sixel and iTerm2 terminals where raster persistence requires resetting underlying buffers:
  ```rust
  pub fn should_clear_on_update(&self) -> bool {
      matches!(
          self.picker.protocol_type(),
          ProtocolType::Sixel | ProtocolType::Iterm2
      )
  }
  ```
- **DEC Mode 2026 Synchronized Output**: Wrap clear + `terminal.draw(...)` in `run_synchronized_update` (`\x1b[?2026h` / `\x1b[?2026l`) so terminals present frames atomically without tearing.
- **Cached Terminal Size**: Query `terminal.size()` once before the main event loop and keep `term_size` updated via `Event::Resize(w, h)` (plus `term_size = f.area().as_size()` inside `terminal.draw`) rather than invoking `crossterm::terminal::size()` (`open("/dev/tty")` + `ioctl` + `close`) on every loop tick.

### 3. Input Repeat Coalescing

- **Event Coalescing**: Drain Crossterm's event buffer using non-blocking polls to coalesce rapid keyboard repeat strokes (like continuous panning/zooming) before drawing a frame:
  ```rust
  if event::poll(poll_timeout)? {
      events.clear();
      events.push(event::read()?);
      while event::poll(Duration::from_millis(0))? {
          events.push(event::read()?);
      }
      for ev in events.drain(..) {
          if let Some(action) = translate_event(app.palette_mode, &ev) {
              app.apply_action(action, term_size.height);
          }
      }
  }
  ```
- **Loop Prevention**: Ignore stdin queries/responses written by Kitty/Sixel graphics protocols. Only trigger redrawing (`draw_needed = true`) on user input (`Key`, `Mouse`, `Paste`), terminal `Resize` events, or when channel ticks deliver new frames.
