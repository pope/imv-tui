---
name: image-processing
description: Guidelines on aspect ratio crop mapping, high-performance resizing, panning margins, cache retention, and memory-only image decoding.
---

# Image Processing, Scaling, and Caching

### 1. In-Memory Aspect-Ratio Crop Mapping

Standard terminal widgets (like `StatefulImage`) automatically scale input images to fit target layouts while maintaining the image's aspect ratio.

- If you zoom in by cropping the original image but preserve the original aspect ratio, the widget layout engine will continue rendering the zoomed image with black bars.
- To make the zoomed image fill the terminal window, the cropped sub-image **must have the aspect ratio of the terminal widget**.
- Calculate the crop window dimensions in original image space using target widget pixel dimensions, scaled by the combined zoom:
  ```rust
  let widget_w_px = widget_width_cells as f64 * cell_width;
  let widget_h_px = widget_height_cells as f64 * cell_height;
  let fit_scale = widget_w_px / img_width; // scaled to fit
  let scale = fit_scale * zoom_factor;

  let crop_w = (widget_w_px / scale).round() as u32;
  let crop_h = (widget_h_px / scale).round() as u32;
  ```
  Always clamp `crop_w` and `crop_h` individually to the original image dimensions.

### 2. Zooming Beyond 1:1 Pixel Ratios

To bypass the maximum rendering size clamping of terminal graphics renderers:

- Resize the cropped viewport in-memory to match target widget pixels using `image::imageops::resize` prior to sending it to the rendering protocol:
  ```rust
  let target_w = (crop_w as f64 * scale).round() as u32;
  let target_h = (crop_h as f64 * scale).round() as u32;

  // Resize in-memory
  let resized = cropped.resize(target_w, target_h, image::imageops::FilterType::Nearest);
  ```
  Using the **Nearest Neighbor** filter ensures high performance (\<1ms) and pixel-perfect inspection.

### 3. Screen-Canvas Panning & Off-Screen Margins

When panning an image past its boundaries (where parts of the viewport show empty black padding):

- Crop only the visible intersection of the crop box and the original image.
- Resize it to its final screen-pixel scale, and overlay/paste it onto a blank (rgba/transparent) **screen-size canvas buffer** at computed offset positions:
  ```rust
  let mut canvas = RgbaImage::new(target_width, target_height);
  image::imageops::overlay(&mut canvas, &resized_intersection, paste_x, paste_y);
  ```
- **High-Performance Resizing**: Do not create the canvas at high original-image resolutions. Always crop the intersection in original space first, resize the cropped part to screen-pixel dimensions, and then overlay it onto a screen-resolution canvas.
- **Corner Center Panning Limits**: Clamp `pan_offset` boundaries to `img_width / 2` and `img_height / 2` so that at least 1/4 of the image remains visible.

### 4. Sliding Window Prefetch Cache ($2N+1$)

- **Sliding Window bounds**: Maintain a sliding window of size $N=2$ (caches the current image + 2 preceding + 2 succeeding images).
- **Dynamic cache retention**: Prune the prefetch cache using a dynamic range check (`cache.retain(|idx, _| window_indices.contains(idx))`) on every navigation.
- **Out-of-order check**: Only insert a returned prefetch image into the cache if its index is still within the active window when the loader thread returns it.
- **Thumbnail pre-caching**: Always load and save both full-resolution image (`image`) and thumbnail placeholder (`thumbnail`) inside `CachedImage` cache entries.
- **Coalesce Locking**: Lock the prefetch cache mutex exactly once during prefetch triggering to prune, check presence, and collect dispatch lists to minimize synchronization overhead.

### 5. EXIF & One-Hit Disk Loads

- Use `into_decoder()` to read EXIF tags, then call `apply_orientation()` to align the image coordinates prior to computing any layout boundaries.
- **Load Once**: Read local file contents into an in-memory buffer (`std::fs::read`) exactly once, and wrap in `std::io::Cursor` for all format and metadata parsing steps to avoid double reads.
- **EXIF Bounds Safety**: Prevent integer overflows in EXIF metadata indexing (e.g. `tiff_start + offset + length`) by utilizing checked additions (`checked_add`), protecting against slicing/debugging panics from malformed headers.
- **In-place Adjustments**: Apply color changes on the canvas using `brighten_in_place` and `contrast_in_place` to avoid cloning intermediate buffers.
- **Zero-copy Zip decoders**: Instantiate CBZ format readers using shared cursor reference `Cursor::new(&buffer)`.

### 6. Pipeline Synchronization & Bounds Safety

- **Sequence Validation**: Use sequence tags (`sequence`) in resize requests and responses to prevent stale resize returns from overwriting the active view protocol during fast navigation. Filter loader thread queues so prefetch responses bypass interactive sequence comparisons.
- **Pasting Bounds Clamping**: Always clamp/crop drawing segments relative to the target screen canvas to prevent out-of-bounds `copy_from` errors caused by sub-pixel rounding mismatches.
