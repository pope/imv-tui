---
name: image-processing
description: Guidelines on aspect ratio crop mapping, high-performance resizing, SIMD adjustments, lock-free prefetch cache retention, and memory-only image decoding.
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
  Clamp `crop_w` and `crop_h` to the original image dimensions using domain types (`CropBox`).

### 2. High-Performance Zooming & Panning

- **Zoom > 1:1**: Crop the viewport in original space, then scale in-memory to screen-pixel size using the high-speed Nearest Neighbor filter (`FilterType::Nearest`).
- **Panning**: Crop the visible intersection of the crop box and original image, scale it, and paint/overlay onto a transparent screen-resolution canvas at computed offsets.
- **Panning Limits**: Clamp `PanOffset` boundaries to `img_width / 2` and `img_height / 2` (ensuring at least 25% of the image remains visible).
- **Direct Row-Slice Canvas Copying**: Avoid per-pixel copy loops. Copy contiguous horizontal rows using direct slice copies into the screen canvas:
  ```rust
  screen_canvas[dest_start..dest_end].copy_from_slice(&src_rgba[src_start..src_end]);
  ```
- **5-Filter SIMD Scaling Lineup**: Cycle scaling filters in monotonic order of support radius: `Nearest -> Box -> Linear -> Mitchell -> Lanczos -> Nearest`. Mapped via `to_fir_resize_alg` to `fir::ResizeAlg::Nearest` and pure SIMD `fir::ResizeAlg::Convolution(...)` (using custom analytical kernels for Mitchell and Lanczos with $2\\times2$ neighborhood anti-ringing clamping on magnification).

### 3. Lock-Free Prefetch Caching ($2N+1$ Sliding Window)

- **Sliding Window Bounds**: Maintain a sliding window around `current_index` of size $N=2$ (caches the current image + 2 preceding + 2 succeeding visible indices).
- **Dynamic Pruning**: Call `cache.retain_window(&window_indices)` on every navigation to discard entries outside the active window while preserving active allocations.
- **No Eviction on Hit**: Never evict cached images on hit; return shared references wrapped in `Arc<DecodedImage>`.
- **Encapsulated `CachedThumbnail`**: Store `CachedImage` containing `image: Option<Arc<DecodedImage>>` and `thumbnail: Option<CachedThumbnail>`.
- **Thumbnail Placeholders**: If a cache entry contains `thumbnail: Some(CachedThumbnail)`, display it immediately as a fast low-resolution placeholder while the background worker decodes the full-resolution image.

### 4. SIMD Adjustments & Precomputed LUTs

- **16-Byte Contiguous SIMD Chunking**: In `apply_brightness_simd`, process 16 bytes (4 RGBA pixels) per step using saturating arithmetic with a mask that zeros out the 4th (Alpha) lane:
  ```rust
  let mask: [u8; 16] = [
      delta, delta, delta, 0,
      delta, delta, delta, 0,
      delta, delta, delta, 0,
      delta, delta, delta, 0,
  ];
  let (chunks16, tail) = pixels.as_chunks_mut::<16>();
  for chunk in chunks16 {
      for i in 0..16 {
          chunk[i] = chunk[i].saturating_add(mask[i]);
      }
  }
  ```
- **Hoisted 256-Entry LUT**: For contrast adjustments, construct the 256-entry lookup table once via `apply_adjustments_lut` and execute it across all active rows *before* copying into the canvas, avoiding re-generating tables inside row loops.
- **Direct EXIF Metadata**: Read EXIF metadata directly from the source buffer via `kamadak-exif` with `checked_add` bounds checks to prevent slicing panics on corrupted headers.
- **Zero-Copy Conversion**: Convert `DynamicImage` buffers to RGBA8 via `into_rgba8_fast()`, avoiding buffer reallocations when the underlying container is already `ImageRgba8`.

### 5. Thread Coordination & Pipeline Invariants

- **Sequence Numbers & Load Phases**: Coordinate background decoder threads using monotonically increasing sequence IDs and `LoadPhase`. Responses with stale sequence numbers are silently dropped.
- **Canvas Clamping**: Always defensively clamp source and destination coordinates to prevent out-of-bounds slice indexing when drawing resized sub-regions.
