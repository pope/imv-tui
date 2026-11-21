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
- **RGB8 End-to-End Preservation**: Keep opaque images in `DynamicImage::ImageRgb8` through `fast_resize` (using `clamp_anti_ringing_rgb_in_place` when magnified and `apply_adjustments_lut_rgb` for brightness/contrast) so full-viewport Kitty SHM/Direct/iTerm2 encoders transmit `f=24` without intermediate RGBA32 unpacking.
- **Direct-to-SHM Parallel Compositing**: For letterboxed/panned frames under `ActiveProtocol::Kitty(KittyTransferMode::SharedMemory)`, composite active rows directly into the OS-zero-filled `mmap`'d `/dev/shm` buffer via `KittyShmProtocol::from_rgba_writer` + `composite_resized_into_rgba_slice` (parallelized across horizontal bands via `rayon` above `PARALLEL_COMPOSITE_MIN_PIXELS = 65_536`), leaving transparent margins as untouched tmpfs zero pages.
- **5-Filter SIMD Scaling Lineup**: Cycle scaling filters in monotonic order of support radius: `Nearest -> Box -> Linear -> Mitchell -> Lanczos -> Nearest`. Mapped via `to_fir_resize_alg` to `fir::ResizeAlg::Nearest` and pure SIMD `fir::ResizeAlg::Convolution(...)` (using custom analytical kernels for Mitchell and Lanczos with $2\\times2$ separable SIMD anti-ringing clamping on magnification).

### 3. Lock-Free Prefetch Caching ($2N+1$ Sliding Window)

- **Sliding Window Bounds**: Maintain a sliding window around `current_index` of size $N=2$ (caches the current image + 2 preceding + 2 succeeding visible indices).
- **Dynamic Pruning & Off-Thread Deallocation**: Call `cache.retain_window(&window_indices)` on every navigation to discard entries outside the active window while preserving active allocations. Evicted `CachedFullImage` `Arc<DynamicImage>` buffers are sent to a lazily spawned `"imv-dropper"` background thread so synchronous `madvise`/`munmap` calls never hitch the main UI loop.
- **No Eviction on Hit**: Never evict cached images on hit; return shared references wrapped in `Arc<DecodedImage>`.
- **Encapsulated `CachedThumbnail`**: Store `CachedImage` containing `full: Option<CachedFullImage>` and `thumbnail: ThumbnailState<CachedThumbnail>`.
- **Thumbnail Placeholders**: If a cache entry contains `thumbnail: ThumbnailState::Loaded(CachedThumbnail)`, display it immediately as a fast low-resolution placeholder while the background worker decodes the full-resolution image.

### 4. SIMD Adjustments & Precomputed LUTs

- **32-Byte Contiguous SIMD Chunking**: In `apply_brightness_simd` (RGBA8, using an opaque `black_box` 4-byte pixel mask to preserve Alpha and prevent LLVM strided scatter stores) and `apply_brightness_simd_rgb` (RGB8), process 32-byte AVX2 chunks with saturating arithmetic.
- **Hoisted 256-Entry LUT**: For contrast adjustments, construct the 256-entry lookup table once via `build_adjustments_lut` (`apply_adjustments_lut` / `apply_adjustments_lut_rgb`) and execute it across active rows *before* compositing, avoiding re-generating tables inside row loops.
- **Cached CBZ Central-Directory Offsets**: Cache per-archive `(PathBuf, len, mtime) -> Arc<HashMap<String, CbzEntryMeta>>` entry offsets (`data_start`, `compressed_size`, `uncompressed_size`, `CbzCompression`) in `src/imaging/raw.rs` so loader workers seek directly to `Stored`/`Deflated` payloads without re-parsing the ZIP central directory per page.
- **Direct EXIF Metadata**: Read EXIF metadata directly from the source buffer via `kamadak-exif` with `checked_add` bounds checks to prevent slicing panics on corrupted headers.
- **Zero-Copy Conversion**: Convert `DynamicImage` buffers to RGBA8 via `into_rgba8_fast()`, avoiding buffer reallocations when the underlying container is already `ImageRgba8`.

### 5. Thread Coordination & Pipeline Invariants

- **Sequence Numbers & Load Phases**: Coordinate background decoder threads using monotonically increasing sequence IDs and `LoadPhase`. Responses with stale sequence numbers are silently dropped.
- **2-Stage Loader Pool (`spawn_loader_pool`, `spawn_resize_worker`)**: Dedicated 1-thread fast-thumbnail coordinator that performs initial header probing, fast EXIF/XDG thumbnail extraction, and job deduplication/reconciliation, feeding `decode_worker_count()` (`2..=5`) parallel full-resolution decode worker threads.
- **Window-Aware Cancellation & In-Flight Promotion**: When navigation updates sequence IDs, obsolete in-flight jobs are cancelled via per-slot atomic cancellation flags (`CancellationToken::from_flag`), while in-flight or pending jobs matching the new active window have their sequence and mode upgraded in-place to fulfill foreground loads without duplicate decoding.
- **Fast-Scroll Cancellation & Thumbnail Fast-Path**: During rapid scrolling (`LoaderMode::ThumbnailOnly`), full-resolution decoding is deferred until scrolling settles (`App::tick()`) and any active background full decodes are cancelled. Stepping onto an index already queued via background `Prefetch` never skips fast thumbnail extraction: the `<1ms` EXIF/XDG thumbnail is dispatched immediately to the UI while updating the in-flight/pending slot.
- **Stage-Gated Parallel Prefetches**: Workers stage-gate background `Prefetch` decodes behind active foreground full-resolution decodes via `can_pop_job`, ensuring foreground images receive maximum CPU and memory bandwidth before parallel prefetch workers fan out across queued jobs.
- **Lock-Free Worker Aborts**: Worker threads check cooperative cancellation via `cancel.is_cancelled() || (slot_seq.load(Ordering::Acquire) as usize) < active_seq.load(Ordering::Acquire)` completely lock-free without acquiring queue mutexes during decode loops.
- **Scoped Parallel Directory Scanning (`filter_image_candidates`)**: Parallelizes `--magic` byte verification across candidate files using `std::thread::scope` when candidate count $\\ge 64$, chunking work across up to 8 threads while maintaining exact deterministic file ordering.
- **Canvas Clamping**: Always defensively clamp source and destination coordinates to prevent out-of-bounds slice indexing when drawing resized sub-regions.
