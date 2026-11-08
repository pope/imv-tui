# Agent Guidelines & Learnings: `imv-tui`

This workspace contains specialized agent skills under `.agents/skills/` to provide detailed implementation guidelines and learnings:

- **\[terminal-graphics\](file:///.agents/skills/terminal_graphics/SKILL.md)**: Sixel/Kitty redrawing protocols, selective clearing via `ScreenClear`, and input repeat coalescing.
- **\[image-processing\](file:///.agents/skills/image_processing/SKILL.md)**: Aspect-ratio crop mapping, zooming beyond 1:1, canvas panning limits, lock-free prefetch cache retention, SIMD brightness, and EXIF metadata decodes.
- **\[tui-architecture\](file:///.agents/skills/tui_architecture/SKILL.md)**: RAII safety guards, pure rendering viewports via `UiModel`, strict one-way DAG, and modular separations.
- **\[code-review\](file:///.agents/skills/code_review/SKILL.md)**: Core code review framework, DAG checks, async boundaries, state machines, and type safety constraints.
- **\[line-editing\](file:///.agents/skills/line_editing/SKILL.md)**: `EditorAction` translation, zero-allocation UTF-8 indexing safety, and `LineEditor` widget logic.
- **\[keybindings\](file:///.agents/skills/keybindings/SKILL.md)**: Unified `InputAction` translation, modifier collision checks, and declarative command metadata schemas.
- **\[testing\](file:///.agents/skills/testing/SKILL.md)**: Guidelines for writing unit tests, mocking channels, and clamping validation asserts.

Refer to those files when making modifications in those domains.

______________________________________________________________________

## Workspace Rules & Coding Constraints

1. **Keep Compile Warning-Free**:

   - Always run `cargo check` and `cargo clippy` after changes. The codebase must compile warning-free on the latest stable Rust channel.
   - Zero compiler warnings, zero clippy warnings, and zero `#[allow(dead_code)]` annotations are allowed.

2. **In-place Performance Invariants & Type-Safe State Machines**:

   - Custom clamping and validation types (`Brightness`, `Contrast`, `PanOffset`, `CropBox`, `ZoomFactor`) must be used to enforce domain limits.
   - Type-safe state machine enums (`ResizeState`, `LoadPhase`, `InitialLoadState`, `ScreenClear`, `ScrollCadence`) must be used to enforce application and pipeline invariants at compile time without loose boolean or nullable timestamp flag combinations.
   - Do not perform expensive heap allocations or cloning inside hot loops (e.g. interactive filtering, directory listing, SIMD canvas copies, or layout drawing).

3. **Strict Module DAG (`config + imaging + commands -> ui -> app -> main`) & Pure Viewports**:

   - The draw phase (`src/ui/`) must remain strictly side-effect free, rendering exclusively from `UiModel<'a>` (`src/ui/model.rs`) produced by `App::ui_model(&mut self)` with **zero** `App` dependency.
   - All selection index updates, layout calculations, and channel polling must occur in the state controller (`src/app/`) during input event processing and system updates.
   - `src/commands/` translates raw Crossterm events into pure `InputAction` values via `translate_event(palette_mode, &ev)`, which `App::apply_action` reduces.
   - Lower modules (`src/config/`, `src/imaging/`, `src/commands/`, `src/ui/`) must have **zero** imports from `crate::app`:
     `! git grep "crate::app" src/config/ src/imaging/ src/commands/ src/ui/`.

4. **Pre-Report Verification Checklist**:

   - Before finishing your task and reporting back to the user, always perform the following validation commands:
     - Run `! git grep "pub(crate)" src/ && ! git grep "dead_code" src/ && ! git grep "crate::app" src/config/ src/imaging/ src/commands/ src/ui/`.
     - Run `cargo test` and ensure all unit and integration tests pass.
     - Run `cargo clippy --all-targets -- -D warnings` and address all errors/warnings.
     - Run `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` to verify documentation.
     - Run `nix fmt` to auto-format code changes.
     - Run `cargo build` to verify compilation.

5. **API Visibility & Documentation Coverage**:

   - Enforce compiler-checked documentation coverage by keeping `#![warn(missing_docs)]` active at the crate root.
   - Use standard `pub` for cross-module structures, enums, fields, and functions so visibility is consistent across the binary crate (no `pub(crate)` allowed), while keeping module-internal helpers and state private.
   - All public and internal API surfaces must be thoroughly documented with standard Rust doc comments (`///` and `//!`).
