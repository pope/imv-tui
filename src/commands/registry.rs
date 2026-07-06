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

#[cfg(test)]
mod tests {
    use super::*;
    use strum::IntoEnumIterator;

    #[test]
    fn test_commands_registry_count() {
        let commands = get_commands();
        let expected_count = Command::iter().count();
        assert_eq!(
            commands.len(),
            expected_count,
            "Registry count must match the total number of Command variants"
        );
    }

    #[test]
    fn test_commands_registry_formatting_and_metadata() {
        for pal_cmd in get_commands() {
            // Verify lowercase search text mapping
            let expected_search_text =
                format!("{} {}", pal_cmd.item.name, pal_cmd.item.description).to_lowercase();
            assert_eq!(
                pal_cmd.search_text, expected_search_text,
                "Search text mismatch for cmd {:?}",
                pal_cmd.cmd
            );

            // Verify shortcut formatting representation
            let expected_shortcut_str = pal_cmd
                .item
                .shortcuts
                .map(|bindings| {
                    bindings
                        .iter()
                        .map(|bind| bind.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            assert_eq!(
                pal_cmd.shortcut_str, expected_shortcut_str,
                "Shortcut formatting mismatch for cmd {:?}",
                pal_cmd.cmd
            );

            // Verify non-empty metadata strings
            assert!(
                !pal_cmd.item.name.is_empty(),
                "Command name cannot be empty"
            );
            assert!(
                !pal_cmd.item.description.is_empty(),
                "Command description cannot be empty"
            );
        }
    }

    #[test]
    fn test_commands_palette_name_uniqueness() {
        let mut showable_names = std::collections::HashSet::new();
        for pal_cmd in get_commands() {
            if pal_cmd.item.show_in_palette {
                assert!(
                    showable_names.insert(pal_cmd.item.name),
                    "Duplicate name '{}' detected for command palette entry. All palette command names must be unique.",
                    pal_cmd.item.name
                );
            }
        }
    }
}
