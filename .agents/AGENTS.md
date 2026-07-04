# Agent Guidelines & Learnings: `imv-tui`

This workspace contains specialized agent skills under `.agents/skills/` to provide detailed implementation guidelines and learnings:

- **\[terminal-graphics\](file:///.agents/skills/terminal_graphics/SKILL.md)**: Sixel/Kitty redrawing protocols, selective clearing, double-buffering overlays, and repeat event coalescing.
- **\[image-processing\](file:///.agents/skills/image_processing/SKILL.md)**: Aspect-ratio crop mapping, zooming beyond 1:1, canvas panning limits, cache prefetching, and one-hit memory decodes.
- **\[tui-architecture\](file:///.agents/skills/tui_architecture/SKILL.md)**: RAII safety guards, pure rendering viewports, unified input maps, and modular separations.

Refer to those files when making modifications in those domains.

______________________________________________________________________

## Workspace Rules & Coding Constraints

1. **Keep Compile Warning-Free**:

   - Always run `cargo check` and `cargo clippy` after changes. The codebase must compile warning-free on the latest stable Rust channel.

2. **In-place Performance Invariants**:

   - Custom clamping and validation types (`Brightness`, `Contrast`, `PanOffset`, `CropBox`) must be used to enforce domain limits.
   - Do not perform expensive heap allocations or cloning inside hot loops (e.g. interactive filtering, directory listing, or layout drawing).

3. **No Layout Mutation in Rendering**:

   - The draw phase (`src/ui.rs`) must remain strictly side-effect free.
   - All selection index updates and layout updates must happen in the state controller (`src/app.rs`) during input event processing.

4. **Pre-Report Verification Checklist**:

   - Before finishing your task and reporting back to the user, always perform the following validation commands:
     - Run `cargo clippy` and address all errors/warnings.
     - Run `nix fmt` to auto-format code changes.
     - Run `cargo build --release` to verify compilation and build a release candidate to test with.
