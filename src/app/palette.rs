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

/// Computes the centered scroll offset (`start_idx`) for a palette list of `total_items`
/// with `visible_count` rows and current selection `selected_index`.
#[inline]
pub fn centered_scroll_offset(
    selected_index: usize,
    total_items: usize,
    visible_count: usize,
) -> usize {
    let half_visible = visible_count / 2;
    if total_items <= visible_count || selected_index < half_visible {
        0
    } else if selected_index >= total_items.saturating_sub(half_visible) {
        total_items.saturating_sub(visible_count)
    } else {
        selected_index.saturating_sub(half_visible)
    }
}

/// Computes the page jump step size for palette `PageUp` / `PageDown` navigation.
#[inline]
pub fn palette_page_size(terminal_height: u16, total_items: usize) -> usize {
    let viewport_h = terminal_height.saturating_sub(1);
    let max_h = (viewport_h as f64 * 0.5).round() as u16;
    let palette_h = (total_items.min(u16::MAX as usize) as u16)
        .saturating_add(4)
        .max(12)
        .min(max_h);
    (palette_h as usize).saturating_sub(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_centered_scroll_offset() {
        // total_items <= visible_count: always 0
        assert_eq!(centered_scroll_offset(0, 5, 10), 0);
        assert_eq!(centered_scroll_offset(4, 5, 10), 0);

        // total_items = 20, visible_count = 6 (half_visible = 3)
        // At head
        assert_eq!(centered_scroll_offset(0, 20, 6), 0);
        assert_eq!(centered_scroll_offset(2, 20, 6), 0);
        // In middle
        assert_eq!(centered_scroll_offset(3, 20, 6), 0);
        assert_eq!(centered_scroll_offset(5, 20, 6), 2);
        assert_eq!(centered_scroll_offset(10, 20, 6), 7);
        // Near tail: total_items - half_visible = 17
        assert_eq!(centered_scroll_offset(17, 20, 6), 14);
        assert_eq!(centered_scroll_offset(19, 20, 6), 14);
    }

    #[test]
    fn test_palette_page_size() {
        // Very small terminal height (e.g. 10): viewport_h = 9, max_h = 5, palette_h = 12.min(5) = 5, page_size = 1
        assert_eq!(palette_page_size(10, 100), 1);

        // Standard terminal height (e.g. 50): viewport_h = 49, max_h = 25
        // Small item count (e.g. 2): palette_h = (2+4).max(12).min(25) = 12, page_size = 12 - 4 = 8
        assert_eq!(palette_page_size(50, 2), 8);

        // Large item count (e.g. 100): palette_h = (100+4).max(12).min(25) = 25, page_size = 25 - 4 = 21
        assert_eq!(palette_page_size(50, 100), 21);
    }

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
