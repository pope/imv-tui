---
name: code-review
description: Guidelines and checklists for performing architectural and Rust code reviews in imv-tui.
---

# Code Review Invariants

### 1. Quality, Types, and Invariants

- **Clippy & Nix Fmt**: Workspace must compile 100% warning-free under stable `cargo clippy` and be formatted with `nix fmt`.
- **Type-Driven Safety**: Prevent invalid states by encoding mutually exclusive configurations in custom enums (e.g. using enum states instead of multiple flags like `is_active` and `is_paused`). Prefer newtypes (e.g., `Brightness`, `Contrast`) over raw primitives for clamped values.
- **Architecture & Modular Roles**: Ensure code files follow the modular roles specified in \[`tui-architecture`\](file:///.agents/skills/tui_architecture/SKILL.md). Suggest splitting modules/files (such as views in `src/ui/` or controller state in `src/app/`) as their sizes and responsibilities grow.
- **Explicit Channel Structs**: Channels must pass self-documenting structs (e.g. `ResizeResponse`, `LoaderRequest`) instead of anonymous tuples.

### 2. Performance & Allocation-Free Hot Paths

- **Zero-Allocation Hot Paths**: Strictly avoid heap allocations (like `String` formatting or `Vec` insertions) and unnecessary `.clone()` calls inside performance hot loops (e.g. layout rendering, event repeat coalescing, and interactive search filters).
- **Async Thread I/O Boundaries**: Main UI thread must remain non-blocking. Disk operations, image decodes, resizing, and protocol writes must happen on background workers.
- **Cache Pruning & Shared References**: Prefetch cache must be pruned based on sliding window bounds and hold `Arc<DynamicImage>` for zero-copy sharing.
- **Double-Buffering Clear Protocol**: Trigger `needs_clear_once` when overlays/dialogs are active or changing state to prevent text-overlay clipping on WezTerm/Kitty/Foot protocol redraws.
