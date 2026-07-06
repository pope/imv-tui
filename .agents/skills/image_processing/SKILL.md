---
name: image-processing
description: Guidelines on aspect ratio crop mapping, high-performance resizing, panning margins, cache retention, and memory-only image decoding.
---

# Image Processing, Scaling, and Caching

### 1. Aspect-Ratio Crop Mapping

- **Rule**: To prevent black bars when zooming/cropping, the cropped sub-image must match the aspect ratio of the target terminal widget.
- **Equation**:
  ```rust
  let scale = fit_scale * zoom_factor;
  let crop_w = (widget_width_px / scale).round() as u32;
  let crop_h = (widget_height_px / scale).round() as u32;
  ```
  Clamp `crop_w` and `crop_h` to the original image dimensions.

### 2. High-Performance Zooming & Panning

- **Zoom > 1:1**: Crop the viewport in original space, then scale in-memory to screen-pixel size using the high-speed Nearest Neighbor filter (`FilterType::Nearest`).
- **Panning**: Crop the visible intersection of the crop box and original image, scale it, and paint/overlay onto a transparent screen-resolution canvas at computed offsets.
- **Panning Limits**: Clamp `pan_offset` boundaries to `img_width / 2` and `img_height / 2` (ensuring at least 25% of the image remains visible).

### 3. Prefetch Caching ($2N+1$)

- **Sliding Window bounds**: Maintain a sliding window of size $N=2$ (caches the current image + 2 preceding + 2 succeeding).
- **Dynamic Pruning**: Call `cache.retain(|idx, _| window_indices.contains(idx))` on every navigation.
- **No Eviction on Hit**: Never evict cached images from the prefetch cache on hit; read via cloned references to keep them in memory.
- **Unified Caching**: Cache both background prefetch decodes and active user-directed decodes if they fall within the active window.
- **Thumbnail Placeholders**: If a cache entry only has a thumbnail decoded, display it immediately as a fast low-res placeholder while loading the full resolution image asynchronously in the background.

### 4. Direct EXIF Metadata & Checked Math

- **Direct Parse**: Read EXIF metadata directly from the source buffer via `kamadak-exif` instead of spinning up the standard `image` reader solely to query orientation.
- **Checked Math**: Guard EXIF slice bounds (e.g. `tiff_start + offset + length`) using `checked_add` to prevent slicing panics on corrupted headers.
- **Zero-Copy Conversion**: Convert DynamicImages via `into_rgba8()` instead of copying via `to_rgba8()`.
- **In-place Colors**: Apply adjustments on the canvas using `brighten_in_place`/`contrast_in_place` to avoid buffer allocation.

### 5. Thread Coordination

- **Sequence Verification**: Filter loader queues so that prefetch responses bypass interactive sequence comparisons. Only apply active view updates if the response sequence matches the current application sequence number.
- **Canvas Clamping**: Always clamp/crop drawing segments relative to the screen canvas size to prevent out-of-bounds `copy_from` errors.
