---
name: code-review
description: Guidelines and checklists for performing architectural and Rust code reviews in imv-tui.
---

# Code Review Guidelines for `imv-tui`

This skill provides a systematic code review framework for `imv-tui` to ensure consistent performance, idiomatic Rust architecture, and error-free execution.

______________________________________________________________________

## 1. Code Review Checklist

Before approving any changes or reporting back to the user, ensure the following checklist is completed:

- [ ] **No Warnings / Errors**: The workspace must compile 100% warning-free on the stable Rust channel under `cargo clippy`.
- [ ] **Formatting Invariant**: All Rust source code must be formatted using the workspace auto-formatter (`nix fmt`).
- [ ] **Successful Release Build**: Validate that `cargo build --release` compiles successfully.
- [ ] **Strict Separation of Concerns**: No layout mutations or selections are performed in the drawing path (`src/ui/`).
- [ ] **Async Thread I/O Boundaries**:
  - The main UI thread must remain strictly non-blocking.
  - Disk operations (e.g. `std::fs::metadata`, `std::fs::read`), image decoding, scaling, filtering, and protocol serialization must happen on the background loader/worker threads.
- [ ] **In-place Domain Types**: Domain values and ranges (panning, zoom factors, contrast, brightness) must be represented by custom types (e.g. `Brightness`, `Contrast`, `PanOffset`, `CropBox`) rather than raw primitive integers/floats.
- [ ] **No Tuples in Channels**: Message passing payloads should use explicit self-documenting structs (e.g. `CachedImage`, `ResizeResponse`) rather than positional tuples to prevent type complexity and preserve readability.

______________________________________________________________________

## 2. Architecture & Design Patterns

### Thread-Safe Caching & Prefetching

- Staged cache operations must prune non-window items asynchronously.
- Ensure the `PrefetchCache` uses `CachedImage` wrapping `Arc<DynamicImage>` to prevent expensive heap copying.

### Double-Buffering Clear Protocol

- Clear terminal buffers via `needs_clear_once` when transitions occur while overlays/dialogues (e.g. `PaletteMode::Info`) are active. This prevents text overlay graphics from getting overwritten by protocol-level redraws (WezTerm/Kitty/Foot).

### Structuring Payloads

- Use descriptive channel structs like `ResizeResponse` instead of complex tuples. This simplifies message parsing loops:
  ```rust
  pub struct ResizeResponse {
      pub protocol: StatefulProtocol,
      pub rendered_cells: (u16, u16),
      pub process_duration: std::time::Duration,
      pub protocol_duration: std::time::Duration,
      pub target_width: u32,
      pub target_height: u32,
  }
  ```
