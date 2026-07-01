# Agent Guidelines & Learnings: `imv-tui`

This document outlines key technical insights and architectural learnings gained while developing the `imv-tui` Rust terminal image viewer. Future agents working on this codebase should read and follow these principles.

______________________________________________________________________

## 1. Terminal Graphics & Redrawing (Sixel vs. Kitty)

### The Learning

Terminal image rendering libraries (like `ratatui-image`) display graphics using custom escape protocols (e.g. Kitty Graphic Protocol or Sixel).

- **Kitty** uses referenceable overlay buffers that can be selectively redrawn or removed by the terminal.
- **Sixel** writes graphics directly onto the text grid cells of the terminal screen.
- **Double Buffering Conflict**: `ratatui` uses a double-buffering algorithm to only draw terminal cell changes. When text overlay popups (like the Help menu) are dismissed, the terminal double-buffer may not detect a cell change because Sixel widgets do not output standard space character bytes. This leaves the old text overlay characters frozen on top of the image layer in Sixel terminals (such as Foot).
- **Text Overlay Graphics Erasure Bug (Clear to End of Line)**: When drawing text overlays (e.g. popups) centered on top of active terminal graphics (Kitty or Sixel overlays), the TUI backend (like `ratatui`'s `CrosstermBackend` using `\x1b[K`) optimizes rendering by erasing characters to the right of the print cursor. In Kitty, this erases the graphic overlay from the cursor position to the right edge of the screen, creating an asymmetric visual bug where the left margin shows the image but the right margin is cut off.

### Guidelines for Future Work

- Do not clear the entire terminal buffer on continuous updates (like zooming or panning) as it causes annoying screen flicker.
- When toggling overlay panels (e.g. Help menus) or swapping files/rotations, trigger a centralized `needs_clear` flag.
- Conditionally call `terminal.clear()?` *only* if the active protocol requires it. Check the active protocol using the `Picker`:
  ```rust
  pub fn should_clear_on_update(&self) -> bool {
      matches!(self.picker.protocol_type(), ratatui_image::picker::ProtocolType::Sixel)
  }
  ```
  This keeps Kitty and Halfblock rendering smooth and flicker-free, while ensuring Sixel redraws cleanly.
- **Top-Right HUD Workaround for Clear-to-Right Bug**: To preserve a transparent background view of the image margins while avoiding the clear-to-right visual bug in Sixel/Kitty, position the overlay popup against the top-right corner of the viewport (or offset by exactly 1 cell to look floating). Aligning the popup's right edge near the terminal's right boundary removes any visible right margin columns, neutralizing the line-erasure region while leaving the left margins fully transparent and intact.

______________________________________________________________________

## 2. In-Memory Aspect-Ratio Crop Mapping

### The Learning

Standard terminal widgets (like `StatefulImage`) automatically scale input images to fit target layouts while maintaining the image's aspect ratio.

- If you zoom in by cropping the original image but preserve the original aspect ratio, the widget layout engine will continue rendering the zoomed image with black bars.
- To make the zoomed image fill the terminal window, the cropped sub-image **must have the aspect ratio of the terminal widget**.

### Guidelines for Future Work

Calculate the crop window dimensions in original image space using target widget pixel dimensions, scaled by the combined zoom:

```rust
let widget_w_px = widget_width_cells as f64 * cell_width;
let widget_h_px = widget_height_cells as f64 * cell_height;
let fit_scale = widget_w_px / img_width; // scaled to fit
let scale = fit_scale * zoom_factor;

let crop_w = (widget_w_px / scale).round() as u32;
let crop_h = (widget_h_px / scale).round() as u32;
```

Always clamp `crop_w` and `crop_h` individually to the original image dimensions. This naturally compresses the surrounding empty space (padding) as the user zooms in, creating a seamless transition from a centered fit-screen view to a full-screen crop view.

______________________________________________________________________

## 3. Zooming Beyond 1:1 Pixel Ratios

### The Learning

Many terminal graphic renderers and widget wrappers clamp the maximum rendering size of an image to its actual pixel resolution to prevent pixelation. This blocks the user from zooming in closer than actual size (1:1).

### Guidelines for Future Work

To bypass this limitation, resize the cropped viewport in-memory to match target widget pixels using `image::imageops::resize` prior to sending it to the rendering protocol:

```rust
let target_w = (crop_w as f64 * scale).round() as u32;
let target_h = (crop_h as f64 * scale).round() as u32;

// Resize in-memory
let resized = cropped.resize(target_w, target_h, image::imageops::FilterType::Nearest);
```

Using the **Nearest Neighbor** filter ensures:

1. Extremely high performance (resizing runs in `< 1ms`).
2. Sharp, pixel-perfect rendering suitable for inspection (avoiding blurred interpolation).

______________________________________________________________________

## 4. Key Event Robustness

### The Learning

Cross-platform key event parsing in terminals (especially for combinations like `Shift + /` to send `?`) is notoriously inconsistent across terminal multiplexers and emulators.

### Guidelines for Future Work

- Do not rely solely on `KeyCode::Char('?')` to toggle help menus. Bind both `?` and `/` to toggle help windows.
- Always check `key.kind == KeyEventKind::Press` to ignore release events in terminals that support keyboard enhancement protocols.
