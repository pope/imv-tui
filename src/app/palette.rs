use crate::commands::{PaletteCommand, get_commands};

/// The specific input prompt type.
#[derive(Clone, Copy, PartialEq, Eq)]
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
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PaletteMode {
    /// Palette/prompt overlay is closed.
    Closed,
    /// Searchable commands lookup.
    Command,
    /// Fuzzy search files in local queue.
    File,
    /// Prompt value input box is open.
    Prompt,
    /// Image metadata and statistics dialog is open.
    Info,
}

pub fn filter_files(
    query: &str,
    display_names: &[String],
    display_names_lowercase: &[String],
    matcher: &mut nucleo::Matcher,
    visibility: &[bool],
) -> Vec<(usize, String)> {
    if query.is_empty() {
        return display_names
            .iter()
            .enumerate()
            .filter(|&(idx, _)| visibility.get(idx).copied().unwrap_or(false))
            .map(|(idx, name)| (idx, name.clone()))
            .collect();
    }

    let pattern = nucleo::pattern::Pattern::parse(
        query,
        nucleo::pattern::CaseMatching::Ignore,
        nucleo::pattern::Normalization::Smart,
    );

    #[derive(Clone)]
    struct FileCandidate<'a> {
        index: usize,
        name: &'a str,
    }
    impl<'a> AsRef<str> for FileCandidate<'a> {
        fn as_ref(&self) -> &str {
            self.name
        }
    }

    let candidates: Vec<FileCandidate<'_>> = display_names_lowercase
        .iter()
        .enumerate()
        .filter(|&(index, _)| visibility.get(index).copied().unwrap_or(false))
        .map(|(index, name)| FileCandidate {
            index,
            name: name.as_str(),
        })
        .collect();

    let mut matches = pattern.match_list(candidates, matcher);
    matches.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.index.cmp(&b.0.index)));

    matches
        .into_iter()
        .map(|(candidate, _score)| (candidate.index, display_names[candidate.index].clone()))
        .collect()
}

pub fn filter_commands(query: &str, matcher: &mut nucleo::Matcher) -> Vec<PaletteCommand> {
    if query.is_empty() {
        return get_commands()
            .iter()
            .filter(|cmd| cmd.item.show_in_palette)
            .cloned()
            .collect();
    }

    let pattern = nucleo::pattern::Pattern::parse(
        query,
        nucleo::pattern::CaseMatching::Ignore,
        nucleo::pattern::Normalization::Smart,
    );

    #[derive(Clone)]
    struct CmdCandidate {
        cmd: PaletteCommand,
    }
    impl AsRef<str> for CmdCandidate {
        fn as_ref(&self) -> &str {
            &self.cmd.search_text
        }
    }

    let candidates: Vec<CmdCandidate> = get_commands()
        .iter()
        .filter(|cmd| cmd.item.show_in_palette)
        .map(|cmd| CmdCandidate { cmd: cmd.clone() })
        .collect();

    let mut matches = pattern.match_list(candidates, matcher);
    matches.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| (a.0.cmd.cmd as usize).cmp(&(b.0.cmd.cmd as usize)))
    });

    matches
        .into_iter()
        .map(|(candidate, _score)| candidate.cmd)
        .collect()
}
