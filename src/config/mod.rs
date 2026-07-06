/// Command-line argument parsing.
pub mod cli;
/// Key definitions and crossterm matching logic.
pub mod keys;

/// Position of the status infobar on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfoBarPosition {
    /// Render status HUD at the top boundary.
    Top,
    /// Render status HUD at the bottom boundary.
    Bottom,
    /// Disable status HUD rendering entirely.
    None,
}

impl InfoBarPosition {
    /// Returns the height of the infobar in cells.
    pub fn height(self) -> u16 {
        match self {
            Self::None => 0,
            Self::Top | Self::Bottom => 3,
        }
    }
}

/// Represents the slideshow transition delay state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SlideshowState {
    /// Slideshow mode is inactive/stopped.
    #[default]
    Stopped,
    /// Slideshow mode is playing, transitioning images after the specified delay.
    Playing {
        /// Time delay between image transitions.
        delay: std::time::Duration,
    },
    /// Slideshow mode is paused, keeping the current image displayed but retaining the delay configuration.
    Paused {
        /// Programmed transition delay duration.
        delay: std::time::Duration,
    },
}

impl SlideshowState {
    /// Slideshow is off.
    pub const OFF: Self = Self::Stopped;

    /// Construct SlideshowState from a raw seconds count.
    pub fn new(seconds: u32) -> Self {
        if seconds == 0 {
            Self::Stopped
        } else {
            Self::Playing {
                delay: std::time::Duration::from_secs(seconds as u64),
            }
        }
    }

    /// Access optional slideshow transition delay.
    pub fn delay(self) -> Option<std::time::Duration> {
        match self {
            Self::Stopped => None,
            Self::Playing { delay } | Self::Paused { delay } => Some(delay),
        }
    }

    /// Access raw seconds value (0 if off).
    pub fn seconds(self) -> u32 {
        self.delay().map(|d| d.as_secs() as u32).unwrap_or(0)
    }

    /// If true, slideshow mode is active (either playing or paused).
    pub fn is_active(self) -> bool {
        match self {
            Self::Stopped => false,
            Self::Playing { .. } | Self::Paused { .. } => true,
        }
    }

    /// If true, slideshow is playing.
    pub fn is_playing(self) -> bool {
        matches!(self, Self::Playing { .. })
    }

    /// If true, slideshow is paused.
    pub fn is_paused(self) -> bool {
        matches!(self, Self::Paused { .. })
    }
}

impl std::str::FromStr for SlideshowState {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let val = s.trim().parse::<u32>()?;
        Ok(Self::new(val))
    }
}
