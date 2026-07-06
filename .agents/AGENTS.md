# Agent Guidelines & Learnings: `imv-tui`

This workspace contains specialized agent skills under `.agents/skills/` to provide detailed implementation guidelines and learnings:

- **\[terminal-graphics\](file:///.agents/skills/terminal_graphics/SKILL.md)**: Sixel/Kitty redrawing protocols, selective clearing, and input repeat coalescing.
- **\[image-processing\](file:///.agents/skills/image_processing/SKILL.md)**: Aspect-ratio crop mapping, zooming beyond 1:1, canvas panning limits, cache prefetching, and EXIF metadata decodes.
- **\[tui-architecture\](file:///.agents/skills/tui_architecture/SKILL.md)**: RAII safety guards, pure rendering viewports, and modular separations.
- **\[code-review\](file:///.agents/skills/code_review/SKILL.md)**: Core code review framework, validation check bounds, async boundaries, and type safety constraints.
- **\[line-editing\](file:///.agents/skills/line_editing/SKILL.md)**: Line editor input delegation, UTF-8 indexing bounds safety, and negation overflow protections.
- **\[keybindings\](file:///.agents/skills/keybindings/SKILL.md)**: Unified event matching, modifier collision checks, and declarative command metadata schemas.
- **\[testing\](file:///.agents/skills/testing/SKILL.md)**: Guidelines for writing unit tests, mocking channels, and clamping validation asserts.

Refer to those files when making modifications in those domains.

______________________________________________________________________

## Workspace Rules & Coding Constraints

1. **Keep Compile Warning-Free**:

   - Always run `cargo check` and `cargo clippy` after changes. The codebase must compile warning-free on the latest stable Rust channel.

2. **In-place Performance Invariants**:

   - Custom clamping and validation types (`Brightness`, `Contrast`, `PanOffset`, `CropBox`) must be used to enforce domain limits.
   - Do not perform expensive heap allocations or cloning inside hot loops (e.g. interactive filtering, directory listing, or layout drawing).

3. **No Layout Mutation in Rendering**:

   - The draw phase (`src/ui/`) must remain strictly side-effect free.
   - All selection index updates and layout updates must happen in the state controller (`src/app/`) during input event processing.

4. **Pre-Report Verification Checklist**:

   - Before finishing your task and reporting back to the user, always perform the following validation commands:
     - Run `cargo clippy` and address all errors/warnings.
     - Run `nix fmt` to auto-format code changes.
     - Run `cargo build --release` to verify compilation and build a release candidate to test with.
