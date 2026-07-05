use crossterm::event;
use strum::IntoEnumIterator;

pub mod registry;

pub use crate::config::keys::KeyDef;
pub use registry::{PaletteCommand, get_commands};

/// User-facing metadata for a Command.
#[derive(Debug, Clone, Copy)]
pub struct CommandItem {
    /// Human-readable name of the command.
    pub name: &'static str,
    /// Detailed description of what the command does.
    pub description: &'static str,
    /// Whether this command should be searchable/listed inside the command palette.
    pub show_in_palette: bool,
    /// Configured shortcuts/triggers associated with this command.
    pub shortcuts: Option<&'static [KeyDef]>,
}

/// Executable application commands/actions.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::EnumIter)]
pub enum Command {
    /// Exit the application.
    Quit,
    /// Display the next image in the active queue.
    NextImage,
    /// Display the previous image in the active queue.
    PreviousImage,
    /// Zoom in on the current viewport.
    ZoomIn,
    /// Zoom out on the current viewport.
    ZoomOut,
    /// Predefined step-wise zoom in.
    PredefinedZoomIn,
    /// Predefined step-wise zoom out.
    PredefinedZoomOut,
    /// Scale image to 100% zoom (1:1 actual pixels).
    ActualSize,
    /// Reset the zoom factor, pan, brightness, and contrast.
    ResetView,
    /// Brighten the image (increment).
    IncreaseBrightness,
    /// Contrast the image (increment).
    IncreaseContrast,
    /// Pan view left.
    PanLeft,
    /// Pan view right.
    PanRight,
    /// Rotate the image 90 degrees clockwise in-memory.
    RotateClockwise,
    /// Rotate the image 90 degrees counter-clockwise in-memory.
    RotateCounterClockwise,
    /// Cycle through the next image scaling filter.
    NextFilter,
    /// Cycle through the next image scale mode.
    CycleScaleMode,
    /// Open the command search palette.
    CommandPalette,
    /// Open the file fuzzy search palette.
    FileSearch,
    /// Mark the current image as a Pick.
    MarkPick,
    /// Mark the current image as a Reject.
    MarkReject,
    /// Remove any pick/reject flags from the current image.
    Unflag,
    /// Cycle the display view mode filter.
    CycleView,
    /// Set view mode to Unflagged + Picks.
    SetViewDefault,
    /// Set view mode to Unflagged.
    SetViewUnflagged,
    /// Set view mode to Picks.
    SetViewPicks,
    /// Set view mode to Rejects.
    SetViewRejects,
    /// Set view mode to All.
    SetViewAll,

    // Non-help/derived commands:
    /// Force nearest-neighbor scaling.
    SetFilterNearest,
    /// Force linear scaling.
    SetFilterLinear,
    /// Force cubic scaling.
    SetFilterCubic,
    /// Force mitchell scaling.
    SetFilterMitchell,
    /// Force gaussian scaling.
    SetFilterGaussian,
    /// Force lanczos scaling.
    SetFilterLanczos,
    /// Force hamming scaling.
    SetFilterHamming,
    /// Navigate to a specific image index by number.
    GoToImage,
    /// Set a precise brightness value.
    SetBrightness,
    /// Set a precise contrast percentage.
    SetContrast,
    /// Set scaling mode to None.
    SetScaleNone,
    /// Set scaling mode to Shrink.
    SetScaleShrink,
    /// Set scaling mode to Fit.
    SetScaleFit,
    /// Set scaling mode to Crop.
    SetScaleCrop,
    /// Darken the image (decrement).
    DecreaseBrightness,
    /// De-contrast the image (decrement).
    DecreaseContrast,
    /// Pan view up.
    PanUp,
    /// Pan view down.
    PanDown,
    /// Increase slideshow timer duration.
    SlideshowIncrease,
    /// Decrease slideshow timer duration.
    SlideshowDecrease,
    /// Set exact slideshow duration in seconds.
    SetSlideshow,
    /// Toggle the image details and statistics info dialog.
    ShowInfo,
    /// Toggle showing the low-res EXIF thumbnail placeholder only (for testing).
    ToggleThumbnail,
    /// Set infobar position to Top.
    SetInfoBarTop,
    /// Set infobar position to Bottom.
    SetInfoBarBottom,
    /// Set infobar position to None.
    SetInfoBarNone,
    /// Cycle infobar position.
    CycleInfoBar,
}

impl Command {
    pub fn from_event(event: &event::Event) -> Option<Self> {
        Self::iter().find(|cmd| {
            let def = cmd.get_metadata();
            let bindings = def.shortcuts.unwrap_or(&[]);
            bindings.iter().any(|bind| bind.matches(event))
        })
    }

    pub fn get_metadata(self) -> CommandItem {
        match self {
            Self::ResetView => CommandItem {
                name: "Reset View",
                description: "Reset View",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('r')]),
            },
            Self::ActualSize => CommandItem {
                name: "Actual Size",
                description: "Actual Size",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('a')]),
            },
            Self::RotateClockwise => CommandItem {
                name: "Rotate Clockwise",
                description: "Rotate CW 90°",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('e'), KeyDef::Char('R'), KeyDef::Char('>')]),
            },
            Self::RotateCounterClockwise => CommandItem {
                name: "Rotate Counter-Clockwise",
                description: "Rotate CCW 90°",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('E'), KeyDef::Char('<')]),
            },
            Self::NextImage => CommandItem {
                name: "Next Image",
                description: "Next image",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('n'), KeyDef::Char(' '), KeyDef::Char(']')]),
            },
            Self::PreviousImage => CommandItem {
                name: "Previous Image",
                description: "Previous image",
                show_in_palette: true,
                shortcuts: Some(&[
                    KeyDef::Char('p'),
                    KeyDef::Code(event::KeyCode::Backspace),
                    KeyDef::Char('['),
                ]),
            },
            Self::ZoomIn => CommandItem {
                name: "Zoom In",
                description: "Zoom In",
                show_in_palette: true,
                shortcuts: Some(&[
                    KeyDef::Char('i'),
                    KeyDef::Char('+'),
                    KeyDef::Char('='),
                    KeyDef::ScrollUp,
                ]),
            },
            Self::ZoomOut => CommandItem {
                name: "Zoom Out",
                description: "Zoom Out",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('o'), KeyDef::Char('-'), KeyDef::ScrollDown]),
            },
            Self::Quit => CommandItem {
                name: "Quit",
                description: "Quit",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('q'), KeyDef::Code(event::KeyCode::Esc)]),
            },
            Self::SetFilterNearest => CommandItem {
                name: "Set Filter: Nearest",
                description: "Use Nearest Neighbor scaling (sharp, pixelated)",
                show_in_palette: true,
                shortcuts: None,
            },
            Self::SetFilterLinear => CommandItem {
                name: "Set Filter: Linear",
                description: "Use Bilinear scaling",
                show_in_palette: true,
                shortcuts: None,
            },
            Self::SetFilterCubic => CommandItem {
                name: "Set Filter: Cubic",
                description: "Use Bicubic scaling (Catmull-Rom)",
                show_in_palette: true,
                shortcuts: None,
            },
            Self::SetFilterMitchell => CommandItem {
                name: "Set Filter: Mitchell",
                description: "Use Mitchell-Netravali scaling",
                show_in_palette: true,
                shortcuts: None,
            },
            Self::SetFilterGaussian => CommandItem {
                name: "Set Filter: Gaussian",
                description: "Use Gaussian scaling",
                show_in_palette: true,
                shortcuts: None,
            },
            Self::SetFilterLanczos => CommandItem {
                name: "Set Filter: Lanczos",
                description: "Use Lanczos3 scaling (high quality)",
                show_in_palette: true,
                shortcuts: None,
            },
            Self::SetFilterHamming => CommandItem {
                name: "Set Filter: Hamming",
                description: "Use Hamming scaling",
                show_in_palette: true,
                shortcuts: None,
            },
            Self::NextFilter => CommandItem {
                name: "Next Filter",
                description: "Next scaling filter",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('S')]),
            },
            Self::GoToImage => CommandItem {
                name: "Go to Image",
                description: "Jump to a specific image index",
                show_in_palette: true,
                shortcuts: None,
            },
            Self::SetBrightness => CommandItem {
                name: "Set Brightness",
                description: "Set image brightness to an absolute value or offset (e.g. 50, +10, -10)",
                show_in_palette: true,
                shortcuts: None,
            },
            Self::SetContrast => CommandItem {
                name: "Set Contrast",
                description: "Set image contrast percentage to an absolute value or offset (e.g. 20, +5, -5)",
                show_in_palette: true,
                shortcuts: None,
            },
            Self::SetScaleNone => CommandItem {
                name: "Set Scale: None",
                description: "Do not scale the image (show at actual size 1:1)",
                show_in_palette: true,
                shortcuts: None,
            },
            Self::SetScaleShrink => CommandItem {
                name: "Set Scale: Shrink to Fit",
                description: "Scale larger images down to fit, leave smaller images untouched",
                show_in_palette: true,
                shortcuts: None,
            },
            Self::SetScaleFit => CommandItem {
                name: "Set Scale: Fit View",
                description: "Scale images up or down to fit the viewport perfectly",
                show_in_palette: true,
                shortcuts: None,
            },
            Self::SetScaleCrop => CommandItem {
                name: "Set Scale: Crop to Fill",
                description: "Scale images to completely fill the viewport (cropping excess)",
                show_in_palette: true,
                shortcuts: None,
            },
            Self::CycleScaleMode => CommandItem {
                name: "Cycle Scale Mode",
                description: "Cycle scale mode",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('s')]),
            },
            Self::PredefinedZoomIn => CommandItem {
                name: "Predefined Zoom In",
                description: "Predefined Zoom In",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('I')]),
            },
            Self::PredefinedZoomOut => CommandItem {
                name: "Predefined Zoom Out",
                description: "Predefined Zoom Out",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('O')]),
            },
            Self::IncreaseBrightness => CommandItem {
                name: "Increase Brightness",
                description: "Increase brightness by 10",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('b')]),
            },
            Self::DecreaseBrightness => CommandItem {
                name: "Decrease Brightness",
                description: "Decrease brightness by 10",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('B')]),
            },
            Self::IncreaseContrast => CommandItem {
                name: "Increase Contrast",
                description: "Increase contrast by 5%",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('c')]),
            },
            Self::DecreaseContrast => CommandItem {
                name: "Decrease Contrast",
                description: "Decrease contrast by 5%",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('C')]),
            },
            Self::PanLeft => CommandItem {
                name: "Pan Left",
                description: "Pan view left",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('h'), KeyDef::Code(event::KeyCode::Left)]),
            },
            Self::PanRight => CommandItem {
                name: "Pan Right",
                description: "Pan view right",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('l'), KeyDef::Code(event::KeyCode::Right)]),
            },
            Self::PanUp => CommandItem {
                name: "Pan Up",
                description: "Pan view up",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('k'), KeyDef::Code(event::KeyCode::Up)]),
            },
            Self::PanDown => CommandItem {
                name: "Pan Down",
                description: "Pan view down",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('j'), KeyDef::Code(event::KeyCode::Down)]),
            },
            Self::CommandPalette => CommandItem {
                name: "Command Palette",
                description: "Open the Command Palette to search commands & shortcuts",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char(':'), KeyDef::Char('?'), KeyDef::Char('/')]),
            },
            Self::FileSearch => CommandItem {
                name: "File Search",
                description: "File Search",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('f')]),
            },
            Self::MarkPick => CommandItem {
                name: "Mark Pick",
                description: "Mark the current image as a Pick",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('z')]),
            },
            Self::MarkReject => CommandItem {
                name: "Mark Reject",
                description: "Mark the current image as a Reject",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('x')]),
            },
            Self::Unflag => CommandItem {
                name: "Unflag Image",
                description: "Remove any pick/reject flags from the current image",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('u')]),
            },
            Self::CycleView => CommandItem {
                name: "Cycle View Filter",
                description: "Cycle display view mode (Unflagged + Picks, Unflagged Only, Picks Only, Rejects Only, All)",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('v')]),
            },
            Self::SetViewDefault => CommandItem {
                name: "Set View: Unflagged + Picks",
                description: "Set display view mode to Unflagged + Picks",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('1')]),
            },
            Self::SetViewUnflagged => CommandItem {
                name: "Set View: Unflagged Only",
                description: "Set display view mode to Unflagged Only",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('2')]),
            },
            Self::SetViewPicks => CommandItem {
                name: "Set View: Picks Only",
                description: "Set display view mode to Picks Only",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('3')]),
            },
            Self::SetViewRejects => CommandItem {
                name: "Set View: Rejects Only",
                description: "Set display view mode to Rejects Only",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('4')]),
            },
            Self::SetViewAll => CommandItem {
                name: "Set View: All Files",
                description: "Set display view mode to All Files",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('5')]),
            },
            Self::SlideshowIncrease => CommandItem {
                name: "Increase Slideshow",
                description: "Increase slideshow by 1s",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('t')]),
            },
            Self::SlideshowDecrease => CommandItem {
                name: "Decrease Slideshow",
                description: "Decrease slideshow by 1s",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('T')]),
            },
            Self::SetSlideshow => CommandItem {
                name: "Set Slideshow",
                description: "Set slideshow duration in seconds or offset (e.g. 5, +1, -1)",
                show_in_palette: true,
                shortcuts: None,
            },
            Self::ShowInfo => CommandItem {
                name: "Show Image Info",
                description: "Toggle the image details and statistics info dialog",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('d')]),
            },
            Self::ToggleThumbnail => CommandItem {
                name: "Toggle Thumbnail Mode",
                description: "Toggle displaying the low-res EXIF thumbnail for testing",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('m')]),
            },
            Self::SetInfoBarTop => CommandItem {
                name: "Set Infobar Top",
                description: "Set status infobar position to top",
                show_in_palette: true,
                shortcuts: None,
            },
            Self::SetInfoBarBottom => CommandItem {
                name: "Set Infobar Bottom",
                description: "Set status infobar position to bottom",
                show_in_palette: true,
                shortcuts: None,
            },
            Self::SetInfoBarNone => CommandItem {
                name: "Set Infobar None",
                description: "Hide the status infobar",
                show_in_palette: true,
                shortcuts: None,
            },
            Self::CycleInfoBar => CommandItem {
                name: "Cycle Infobar Position",
                description: "Cycle status infobar position (top, bottom, none)",
                show_in_palette: true,
                shortcuts: Some(&[KeyDef::Char('V')]),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use strum::IntoEnumIterator;

    #[test]
    fn test_no_shortcut_conflicts() {
        let mut shortcut_to_cmd = HashMap::new();
        for cmd in Command::iter() {
            let metadata = cmd.get_metadata();
            if let Some(shortcuts) = metadata.shortcuts {
                for shortcut in shortcuts {
                    if let Some(existing_cmd) = shortcut_to_cmd.insert(*shortcut, cmd) {
                        panic!(
                            "Shortcut {:?} is duplicated! Registered for both {:?} and {:?}",
                            shortcut, existing_cmd, cmd
                        );
                    }
                }
            }
        }
    }
}
