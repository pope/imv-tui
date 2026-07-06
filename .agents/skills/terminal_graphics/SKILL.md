---
name: terminal-graphics
description: Guidelines and learnings for foot/sixel/kitty terminal graphic rendering, selective visual clearing, and input repeats coalescing in imv-tui.
---

# Terminal Graphics & Redrawing (Sixel vs. Kitty)

### 1. Rendering Protocols & Overlay Conflicts

- **Kitty vs Sixel**: Kitty overlays graphics onto a dedicated buffer; Sixel renders direct graphics over text cells.
- **Double Buffering Bug**: Space characters in text overlays (e.g. Help menu) will freeze on top of Sixel layers upon dismissal. Erasing text lines using cursor operations (like `\x1b[K`) will wipe overlapping Kitty graphics.
- **Redraw Cover Invariant**: Triggering any graphic resize/refresh while an overlay dialog is active will draw the image *over* the text. Do not set `needs_update = true` or trigger resizes for text inputs, menu navigation, or state changes (like pausing slideshows).

### 2. Selective Clearing (Anti-Flicker)

- **Selective Clears**: Never clear the screen globally on continuous updates (zoom/pan) to avoid flicker.
- **Centralized Clears**: Set a centralized `needs_clear_once` flag when opening/closing dialog overlays or rotating/swapping images.
- **Clear Condition**: Call `terminal.clear()?` only on Sixel-protocol terminals (`picker.protocol_type() == ProtocolType::Sixel`) when the new graphic is received:
  ```rust
  pub fn should_clear_on_update(&self) -> bool {
      matches!(self.picker.protocol_type(), ProtocolType::Sixel)
  }
  ```

### 3. Input Repeat Coalescing

- **Event Coalescing**: Drain Crossterm's event buffer using non-blocking polls to coalesce rapid keyboard repeat strokes (like continuous panning/zooming) before drawing a frame:
  ```rust
  if event::poll(Duration::from_millis(50))? {
      let mut events = vec![event::read()?];
      while event::poll(Duration::from_millis(0))? {
          events.push(event::read()?);
      }
      // Process events, update offsets, and run update_protocol once.
  }
  ```
- **Loop Prevention**: Ignore stdin queries/responses written by Kitty/Sixel graphics protocols. Only trigger redrawing (`draw_needed = true`) on user input (`Key`, `Mouse`, `Paste`) or terminal `Resize` events.
