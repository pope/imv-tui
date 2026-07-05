pub mod cli;
pub mod keys;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfoBarPosition {
    Top,
    Bottom,
    None,
}

/// Represents the slideshow transition delay duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SlideshowConfig {
    /// Optional slideshow transition delay. None represents slideshow mode is off.
    pub delay: Option<std::time::Duration>,
}

impl SlideshowConfig {
    /// Slideshow is off.
    pub const OFF: Self = Self { delay: None };

    /// Construct SlideshowConfig from a raw seconds count.
    pub fn new(seconds: u32) -> Self {
        Self {
            delay: if seconds == 0 {
                None
            } else {
                Some(std::time::Duration::from_secs(seconds as u64))
            },
        }
    }

    /// Access raw seconds value (0 if off).
    pub fn seconds(self) -> u32 {
        self.delay.map(|d| d.as_secs() as u32).unwrap_or(0)
    }

    /// If true, slideshow mode is active.
    pub fn is_active(self) -> bool {
        self.delay.is_some()
    }
}

impl std::str::FromStr for SlideshowConfig {
    type Err = std::num::ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let val = s.trim().parse::<u32>()?;
        Ok(Self::new(val))
    }
}
