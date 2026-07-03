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

______________________________________________________________________

## 5. Screen-Canvas Panning and Off-Screen Margins

### The Learning

When panning an image past its boundaries (where parts of the viewport show empty black padding), simple cropping using `img.crop_imm(...)` fails because the crop box coordinates extend outside `[0, img_width]` or `[0, img_height]`.

- To support off-screen margins, we can calculate the **intersection** of the crop box and the original image.
- We crop only the visible intersection, resize it to its final screen-pixel scale, and overlay/paste it onto a blank (rgba/transparent) **screen-size canvas buffer** at computed offset positions:
  ```rust
  let mut canvas = RgbaImage::new(target_width, target_height);
  image::imageops::overlay(&mut canvas, &resized_intersection, paste_x, paste_y);
  ```
- **Automatic Centering**: When zoomed out, this canvas-overlay approach automatically centers the image with symmetric padding on all sides, eliminating any complex cell-level coordinate math in layout widgets.
- **Corner Center Panning Limits**: To prevent the image from being panned completely off-screen, clamp the `pan_offset` boundaries to `img_width / 2` and `img_height / 2`. This guarantees that the center of the viewport can never cross the corners of the image, keeping at least 1/4 of the image visible.

### Guidelines for Future Work

- **High-Performance Resizing**: Do not create the canvas at high original-image resolutions. Always crop the intersection in original space first, resize the cropped part to screen-pixel dimensions, and then overlay it onto a screen-resolution canvas. This avoids processing large buffers.

______________________________________________________________________

## 6. Text Overlays on Top of Graphics (Kitty/Sixel Interaction)

### The Learning

When rendering text components (like the Help menu, Command Palette, or File Search dialogs) on top of active terminal graphic overlays (Kitty protocol):

- **Image Redraw covers Text**: Triggering `update_protocol()` (re-rendering/re-uploading the image) while a text dialog is open causes the graphic layer to cover the text grid. Because of `ratatui`'s double buffering, only the text cells that *changed* will be redrawn on top of the image; static text elements (like borders, titles, or unchanged labels) will remain covered and invisible.
- **Erase Text in Kitty/WezTerm**: To optimize background rendering, `ratatui-image` configures cells occupied by the active image to be skipped by the text writer. Consequently, dismissing/closing a text dialog does not automatically clear the characters (spaces) in double buffered Kitty-protocol terminals (like WezTerm). To cleanly wipe these character cells, you must perform a single unconditional clear of the text grid.

### Guidelines for Future Work

- **No Updates on Typing/Navigation**: Do not set `needs_update = true` or call `update_protocol()` for keystrokes, character entry, or row selection inside dialog inputs.
- **Clear Text Grid on Overlay Dismissal**: When dismissing or toggling an overlay *off* (such as closing the Help panel or hiding the command palette), set an unconditional `needs_clear_once = true` flag. This executes `terminal.clear()?` on the next frame, wiping the text grid clean of overlay characters without deleting the background graphics overlay buffer.

______________________________________________________________________

## 7. Asynchronous Thread Offloading & Arc-Sharing

### The Learning

Image loading/decoding and scale resizing are highly CPU-bound. If done synchronously in the main event loop, they freeze the user interface for several hundred milliseconds.

- **Thread-safe Image Sharing**: To safely send decoded images to background threads without expensive memory cloning (which can take 10ms+ for large buffers), wrap the `DynamicImage` in `std::sync::Arc`.
- **Resize Worker Task Coalescing**: When the user repeats a command quickly (like zoom/pan), the worker queue fills up. To prevent CPU thrashes, the worker thread should drain its channel and only process the absolute latest request:
  ```rust
  if let Ok(req) = resize_rx.recv() {
      let mut latest_req = req;
      while let Ok(next_req) = resize_rx.try_recv() {
          latest_req = next_req;
      }
      let protocol = process_resize(latest_req);
      let _ = protocol_tx.send(protocol);
  }
  ```
- **Persistent Image Loader Worker & Request Coalescing**: Spawning a brand-new OS thread for every image load or prefetch request causes severe thread and disk contention when a user spams keys. To prevent this, offload loading to a persistent background Loader Thread.
  - Coalesce loader requests by draining the channel and filtering out any request with an obsolete `sequence` number.
  - Sort the active request list so that the active viewport load (`is_prefetch == false`) is prioritized and processed first, followed by background prefetches.

### Guidelines for Future Work

- Offload both decoding/loading and scaling to dedicated persistent worker threads.
- Keep a global `current_sequence` number in the state controller. When navigating, increment the sequence and attach it to both active and prefetch requests sent to the Loader Thread. Discard any returned results on the main thread if their sequence is older than `current_sequence`.
- Avoid freezing the TUI during startup/navigation by showing a debounced indicator (e.g. only if loading takes >150ms).

______________________________________________________________________

## 8. Selective Visual Clearing (Anti-Flicker Sixel Logic)

### The Learning

Sixel graphics require clear operations (`terminal.clear()?`) to wipe old frames, but clearing on every frame of a continuous update (like zooming or panning) causes extreme screen flicker.

- If we clear the screen immediately on image load, rotation, or zoom triggers, the terminal will render the old image's layout for one frame before the background thread returns the new protocol.
- This creates double-clearing and redraw stutters.
- **Layout Aspect-Ratio Mismatch**: If `self.rendered_size_cells` is updated on the main loop thread before sending a resize request to the worker thread, the old image protocol is rendered stretched/distorted to the new dimensions for one frame.

### Guidelines for Future Work

- Defer screen clearing until the new protocol is actually received from the worker thread.
- Do not clear the terminal during zooming and panning. Use a selective `clear_on_protocol_receive` flag to restrict screen clearing only to discrete state changes (e.g. loading a new file, resetting views, or rotations).
- **Synchronize Protocol and Cell Layout updates**: Pass target cell layout dimensions `rendered_size_cells` along with the `ResizeRequest` to the background thread. Let the background channel yield a tuple `(StatefulProtocol, (u16, u16))` and apply both at the exact same instant inside the channel receiver. This completely eliminates layout distortion stutters.

______________________________________________________________________

## 9. Input Event Coalescing/Debouncing (Keyboard Repeats)

### The Learning

When holding down keys for continuous actions (like panning), standard keyboard repeat configurations trigger events faster than Sixel escape rendering can complete. This creates a backlog of render inputs, leading to delayed panning responses and visual stutter.

### Guidelines for Future Work

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

______________________________________________________________________

## 10. Native EXIF Orientation Decoding

### The Learning

Normal pixel decoders do not automatically apply EXIF rotation metadata.

### Guidelines for Future Work

Instead of raw `ImageReader::decode()`, use `into_decoder()` to read EXIF tags, then call `apply_orientation()` to align the image coordinates prior to computing any layout boundaries:

```rust
let mut decoder = reader.into_decoder()?;
let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
let mut img = DynamicImage::from_decoder(decoder)?;
img.apply_orientation(orientation);
```

______________________________________________________________________

## 11. Frozen Dialog/Palette Width and Height on Open

### The Learning

Recalculating a dialog or search palette's width and height dynamically on every character input (based on the currently filtered list) results in screen-draw artifacts (border "ghosts") when the dialog box shrinks, and creates a jittery, bouncing visual layout.

### Guidelines for Future Work

- **Freeze Layout Width on Open**: When opening search or command palettes, scan the *unfiltered* list of all possible items once to calculate the maximum text length. Set and freeze `self.palette_width` inside the state constructor.
- **Freeze Layout Height on Open**: Calculate the height of the palette once at open time using the unfiltered item list size: `(total_items + 4)`. Set the minimum height to 12 cells and clamp the maximum height to 50% of the viewport. Lock this value into `self.palette_height` and use it throughout the palette lifecycle.
- **Constraints**: Apply rendering constraints in the draw loop, forcing a minimum dialog width (e.g., 40 cells) and capping the maximum width at a percentage of horizontal screen space (e.g., 75% of screen width). Generate horizontal separator lines dynamically based on this static width.

______________________________________________________________________

## 12. Sliding Window Prefetch Cache ($2N+1$)

### The Learning

Caching only immediate neighbors ($N=1$) results in constant disk reads when navigating back and forth across a small set of files.

### Guidelines for Future Work

- **Sliding Window bounds**: Maintain a sliding window of size $N=2$ (caches the current image + 2 preceding + 2 succeeding images).
- **Dynamic cache retention**: Prune the prefetch cache using a dynamic range check (`cache.retain(|idx, _| window_indices.contains(idx))`) on every navigation.
- **Out-of-order check**: Only insert a returned prefetch image into the cache if its index is still within the active window when the loader thread returns it.
