use crate::commands::{Command, CommandItem};

/// A wrapper struct coupling a Command variant with its CommandItem metadata
/// for searchable presentation in lists and palette lookups.
#[derive(Debug, Clone)]
pub struct PaletteCommand {
    /// The executable command variant.
    pub cmd: Command,
    /// The metadata presentation associated with the command.
    pub item: CommandItem,
    /// Pre-computed lowercase search text format: "name description".
    pub search_text: String,
    /// Pre-computed shortcut formatting string.
    pub shortcut_str: String,
}

/// Returns a static slice of all available command metadata items.
pub fn get_commands() -> &'static [PaletteCommand] {
    static LIST: std::sync::OnceLock<Vec<PaletteCommand>> = std::sync::OnceLock::new();
    LIST.get_or_init(|| {
        <Command as strum::IntoEnumIterator>::iter()
            .map(|cmd| {
                let item = cmd.get_metadata();
                let search_text = format!("{} {}", item.name, item.description).to_lowercase();

                let shortcut_str = item
                    .shortcuts
                    .map(|bindings| {
                        bindings
                            .iter()
                            .map(|bind| bind.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();

                PaletteCommand {
                    cmd,
                    item,
                    search_text,
                    shortcut_str,
                }
            })
            .collect()
    })
}
