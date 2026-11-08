---
name: code-review
description: Guidelines and checklists for performing architectural and Rust code reviews in imv-tui.
---

# Code Review Invariants

### 1. Quality, Types, and Invariants

- **Clippy & Nix Fmt**: Workspace must compile 100% warning-free under stable `cargo clippy --all-targets -- -D warnings` and be formatted with `nix fmt`.
- **Strict Module DAG (`config + imaging + commands -> ui -> app -> main`)**:
  - `src/config/`, `src/imaging/`, `src/commands/`, and `src/ui/` must have **zero** imports from `crate::app`:
    `! git grep "crate::app" src/config/ src/imaging/ src/commands/ src/ui/`.
- **API Visibility & Dead Code Hygiene**:
  - Enforce standard `pub` for cross-module items; zero `pub(crate)` allowed (`! git grep "pub(crate)" src/`).
  - Zero `#[allow(dead_code)]` annotations allowed (`! git grep "dead_code" src/`). Keep `#![warn(missing_docs)]` active and 100% warning-free.
- **Type-Driven Safety & State Machines**: Prevent invalid states by encoding mutually exclusive configurations in custom state machine enums (`ResizeState`, `LoadPhase`, `InitialLoadState`, `ScreenClear`, `ScrollCadence`).
- **Domain Clamping Newtypes**: Prefer newtypes (`Brightness`, `Contrast`, `PanOffset`, `CropBox`, `ZoomFactor`) over raw primitives for bounded domain values.
- **Explicit Channel Structs**: Channels must pass self-documenting structs (`ResizeResponse`, `LoaderRequest`, `LoaderResponse`) instead of anonymous tuples.

### 2. Performance & Allocation-Free Hot Paths

- **Zero-Allocation Hot Paths**: Strictly avoid heap allocations (like `String` formatting or `Vec` insertions) and unnecessary `.clone()` calls inside performance hot loops (e.g. layout rendering, event repeat coalescing, and interactive search filters).
- **SIMD & LUT Hoisting**: Process image adjustments using 16-byte SIMD chunking (`apply_brightness_simd`) and hoisted 256-entry lookup tables (`apply_adjustments_lut`) before copying row slices into screen canvases.
- **Async Thread I/O Boundaries**: Main UI thread must remain non-blocking. Disk operations, image decodes, resizing, and protocol writes must happen on background workers.
- **Lock-Free Cache Pruning**: Prefetch cache must be pruned via `cache.retain_window(&window_indices)` based on sliding window bounds and hold `Arc<DecodedImage>` for zero-copy sharing.
- **Type-Safe Clear Protocol**: Use `ScreenClear` (`None`, `OnProtocolReceive`, `Pending`) to clear obsolete cells on Sixel redraws without double-buffering flashes or unnecessary full-screen clears on continuous operations.
