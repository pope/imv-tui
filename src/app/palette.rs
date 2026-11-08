use crate::commands::get_commands;

/// Maximum retained capacity for idle palette scratch and filter buffers.
pub const MAX_IDLE_PALETTE_CAPACITY: usize = 1024;

/// The specific input prompt type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptType {
    /// Go to specific image index.
    GoToImage,
    /// Adjust image brightness.
    SetBrightness,
    /// Adjust image contrast.
    SetContrast,
    /// Adjust slideshow interval.
    SetSlideshow,
}

/// The state of the top overlay search palette or prompt dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteMode {
    /// Palette/prompt overlay is closed.
    Closed,
    /// Searchable commands lookup.
    Command,
    /// Fuzzy search files in local queue.
    File,
    /// Prompt value input box is open with the given prompt type.
    Prompt(PromptType),
    /// Image metadata and statistics dialog is open.
    Info,
}

/// Filters files matching an optional query string using nucleo fuzzy search and a visibility predicate.
pub fn filter_files(
    query: &str,
    display_names: &[String],
    display_names_lowercase: &[String],
    matcher: &mut nucleo::Matcher,
    is_visible: impl Fn(usize) -> bool,
    out: &mut Vec<usize>,
    scratch: &mut Vec<(u32, usize)>,
) {
    out.clear();
    scratch.clear();

    let q = query.trim();
    if q.is_empty() {
        for idx in 0..display_names.len() {
            if is_visible(idx) {
                out.push(idx);
            }
        }
        return;
    }

    let pattern = nucleo::pattern::Pattern::parse(
        q,
        nucleo::pattern::CaseMatching::Ignore,
        nucleo::pattern::Normalization::Smart,
    );

    let mut utf32_buf = Vec::new();
    for (idx, name) in display_names_lowercase.iter().enumerate() {
        if !is_visible(idx) {
            continue;
        }
        let haystack = nucleo::Utf32Str::new(name, &mut utf32_buf);
        if let Some(score) = pattern.score(haystack, matcher) {
            scratch.push((score, idx));
        }
    }

    scratch.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    out.extend(scratch.iter().map(|&(_, idx)| idx));
    scratch.clear();
}

/// Filters palette commands matching an optional query string using nucleo fuzzy search.
pub fn filter_commands(
    query: &str,
    matcher: &mut nucleo::Matcher,
    out: &mut Vec<usize>,
    scratch: &mut Vec<(u32, usize)>,
) {
    out.clear();
    scratch.clear();

    let q = query.trim();
    let commands = get_commands();

    if q.is_empty() {
        for (idx, cmd) in commands.iter().enumerate() {
            if cmd.item.show_in_palette {
                out.push(idx);
            }
        }
        return;
    }

    let pattern = nucleo::pattern::Pattern::parse(
        q,
        nucleo::pattern::CaseMatching::Ignore,
        nucleo::pattern::Normalization::Smart,
    );

    let mut utf32_buf = Vec::new();
    for (idx, cmd) in commands.iter().enumerate() {
        if !cmd.item.show_in_palette {
            continue;
        }
        let haystack = nucleo::Utf32Str::new(&cmd.search_text, &mut utf32_buf);
        if let Some(score) = pattern.score(haystack, matcher) {
            scratch.push((score, idx));
        }
    }

    scratch.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    out.extend(scratch.iter().map(|&(_, idx)| idx));
    scratch.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_files_visibility_predicate() {
        let mut matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let names = vec![
            "photo1.png".to_string(),
            "photo2.jpg".to_string(),
            "photo3.png".to_string(),
        ];
        let names_lower = vec![
            "photo1.png".to_string(),
            "photo2.jpg".to_string(),
            "photo3.png".to_string(),
        ];
        let mut out = Vec::new();
        let mut scratch = Vec::new();

        // Only index 0 and 2 visible
        filter_files(
            "",
            &names,
            &names_lower,
            &mut matcher,
            |idx| idx == 0 || idx == 2,
            &mut out,
            &mut scratch,
        );
        assert_eq!(out, vec![0, 2]);

        // Search with query
        filter_files(
            "photo3",
            &names,
            &names_lower,
            &mut matcher,
            |idx| idx == 0 || idx == 2,
            &mut out,
            &mut scratch,
        );
        assert_eq!(out, vec![2]);

        // Filtered out by visibility
        filter_files(
            "photo2",
            &names,
            &names_lower,
            &mut matcher,
            |idx| idx == 0 || idx == 2,
            &mut out,
            &mut scratch,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn test_filter_commands_search_and_ordering() {
        let mut matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let mut out = Vec::new();
        let mut scratch = Vec::new();

        filter_commands("", &mut matcher, &mut out, &mut scratch);
        assert!(!out.is_empty());

        filter_commands("quit", &mut matcher, &mut out, &mut scratch);
        assert!(!out.is_empty());
        let top_cmd = &get_commands()[out[0]];
        assert_eq!(top_cmd.cmd, crate::commands::Command::Quit);
    }
}
