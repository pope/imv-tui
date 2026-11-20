//! Command definitions, keybinding registry, line editor, and input action translation.

/// Semantic input actions and event translation logic.
pub mod actions;
/// Cursor-aware line editing buffer and actions.
pub mod line_editor;
/// Commands registry for matching descriptions and shortcut keys.
pub mod registry;

pub use actions::{InputAction, OverlayAction, PaletteMode, PromptType, translate_event};
pub use line_editor::LineEditor;
pub use registry::{Command, PanDirection, get_commands};
