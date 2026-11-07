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

/// Controls whether Kitty graphics protocol payloads use zlib (`o=z`) deflate compression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KittyCompression {
    /// Automatically enable compression only when running over a remote SSH session
    /// (`$SSH_CONNECTION`, `$SSH_CLIENT`, or `$SSH_TTY`).
    #[default]
    Auto,
    /// Always compress Kitty graphics payloads (`o=z`).
    Always,
    /// Never compress Kitty graphics payloads (fastest on local PTYs).
    Never,
}

impl KittyCompression {
    /// Returns `true` if any of `$SSH_CONNECTION`, `$SSH_CLIENT`, or `$SSH_TTY` is set to a non-empty value.
    pub fn is_remote_ssh_session() -> bool {
        ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"]
            .iter()
            .any(|&var| std::env::var_os(var).is_some_and(|val| !val.is_empty()))
    }

    /// Evaluates whether compression should be enabled given whether the session is a remote SSH session.
    pub const fn should_compress_for_env(self, is_remote_ssh: bool) -> bool {
        match self {
            Self::Auto => is_remote_ssh,
            Self::Always => true,
            Self::Never => false,
        }
    }

    /// Determines whether Kitty graphics payloads should be compressed in the current environment.
    pub fn should_compress(self) -> bool {
        self.should_compress_for_env(Self::is_remote_ssh_session())
    }
}

/// Validated CLI protocol configuration encoding protocol-specific options in the type system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolConfig {
    /// Auto-detect terminal protocol; if `--kitty-compress` was explicitly set,
    /// `explicit_kitty_compress` is `Some(...)` and requires the detected protocol to be Kitty.
    Auto {
        /// Explicit `--kitty-compress` override passed on the CLI, if any.
        explicit_kitty_compress: Option<KittyCompression>,
    },
    /// Force Kitty graphics protocol with the specified compression policy.
    Kitty {
        /// Compression policy (`Auto` when `--kitty-compress` was omitted).
        compression: KittyCompression,
    },
    /// Force DEC Sixel graphics protocol.
    Sixel,
    /// Force iTerm2 inline image protocol.
    Iterm2,
    /// Force Unicode halfblocks fallback.
    Halfblocks,
}

impl Default for ProtocolConfig {
    fn default() -> Self {
        Self::Auto {
            explicit_kitty_compress: None,
        }
    }
}

impl ProtocolConfig {
    /// Parses protocol configuration from CLI arguments, validating that protocol-specific options
    /// like `--kitty-compress` are only used with Kitty (or auto-detection).
    pub fn from_cli(
        protocol: Option<ratatui_image::picker::ProtocolType>,
        explicit_kitty_compress: Option<KittyCompression>,
    ) -> Result<Self, String> {
        if explicit_kitty_compress.is_some()
            && protocol.is_some_and(|p| p != ratatui_image::picker::ProtocolType::Kitty)
        {
            return Err(
                "Error: Option --kitty-compress can only be used with the kitty protocol"
                    .to_string(),
            );
        }
        match protocol {
            None => Ok(Self::Auto {
                explicit_kitty_compress,
            }),
            Some(ratatui_image::picker::ProtocolType::Kitty) => Ok(Self::Kitty {
                compression: explicit_kitty_compress.unwrap_or(KittyCompression::Auto),
            }),
            Some(ratatui_image::picker::ProtocolType::Sixel) => Ok(Self::Sixel),
            Some(ratatui_image::picker::ProtocolType::Iterm2) => Ok(Self::Iterm2),
            Some(ratatui_image::picker::ProtocolType::Halfblocks) => Ok(Self::Halfblocks),
        }
    }

    /// Evaluates whether Kitty compression should be requested during terminal initialization.
    pub fn should_enable_kitty_compression(self) -> bool {
        match self {
            Self::Kitty { compression } => compression.should_compress(),
            Self::Auto {
                explicit_kitty_compress,
            } => explicit_kitty_compress
                .unwrap_or(KittyCompression::Auto)
                .should_compress(),
            Self::Sixel | Self::Iterm2 | Self::Halfblocks => false,
        }
    }

    /// Returns the forced `ProtocolType` if explicitly configured on the CLI.
    pub fn forced_protocol_type(self) -> Option<ratatui_image::picker::ProtocolType> {
        match self {
            Self::Auto { .. } => None,
            Self::Kitty { .. } => Some(ratatui_image::picker::ProtocolType::Kitty),
            Self::Sixel => Some(ratatui_image::picker::ProtocolType::Sixel),
            Self::Iterm2 => Some(ratatui_image::picker::ProtocolType::Iterm2),
            Self::Halfblocks => Some(ratatui_image::picker::ProtocolType::Halfblocks),
        }
    }

    /// Resolves the final `ActiveProtocol` based on the detected terminal protocol and compression state.
    pub fn resolve_active_protocol(
        self,
        detected: ratatui_image::picker::ProtocolType,
        kitty_compressed: bool,
    ) -> Result<ActiveProtocol, String> {
        match self {
            Self::Auto {
                explicit_kitty_compress,
            } => {
                if explicit_kitty_compress.is_some()
                    && detected != ratatui_image::picker::ProtocolType::Kitty
                {
                    return Err(format!(
                        "Error: Option --kitty-compress was specified, but detected terminal protocol is {:?} (not Kitty)",
                        detected
                    ));
                }
                match detected {
                    ratatui_image::picker::ProtocolType::Kitty => Ok(ActiveProtocol::Kitty {
                        compressed: kitty_compressed,
                    }),
                    ratatui_image::picker::ProtocolType::Sixel => Ok(ActiveProtocol::Sixel),
                    ratatui_image::picker::ProtocolType::Iterm2 => Ok(ActiveProtocol::Iterm2),
                    ratatui_image::picker::ProtocolType::Halfblocks => {
                        Ok(ActiveProtocol::Halfblocks)
                    }
                }
            }
            Self::Kitty { .. } => Ok(ActiveProtocol::Kitty {
                compressed: kitty_compressed,
            }),
            Self::Sixel => Ok(ActiveProtocol::Sixel),
            Self::Iterm2 => Ok(ActiveProtocol::Iterm2),
            Self::Halfblocks => Ok(ActiveProtocol::Halfblocks),
        }
    }
}

/// Active runtime terminal graphics protocol and its protocol-specific configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActiveProtocol {
    /// Kitty graphics protocol with explicit compression state.
    Kitty {
        /// Whether `o=z` zlib deflate compression is enabled.
        compressed: bool,
    },
    /// DEC Sixel graphics protocol.
    Sixel,
    /// iTerm2 inline images protocol.
    Iterm2,
    /// Unicode halfblock character fallback.
    #[default]
    Halfblocks,
}

impl ActiveProtocol {
    #[cfg(test)]
    /// Constructs an `ActiveProtocol` from a `ProtocolType` and a Kitty compression state.
    pub fn from_protocol_type(
        proto: ratatui_image::picker::ProtocolType,
        compressed: bool,
    ) -> Self {
        match proto {
            ratatui_image::picker::ProtocolType::Kitty => Self::Kitty { compressed },
            ratatui_image::picker::ProtocolType::Sixel => Self::Sixel,
            ratatui_image::picker::ProtocolType::Iterm2 => Self::Iterm2,
            ratatui_image::picker::ProtocolType::Halfblocks => Self::Halfblocks,
        }
    }

    /// Returns the descriptive label for Stats for Nerds, indicating compression only when active.
    pub const fn display_label(self) -> &'static str {
        match self {
            Self::Kitty { compressed: true } => "Kitty (Compressed)",
            Self::Kitty { compressed: false } => "Kitty",
            Self::Sixel => "Sixel",
            Self::Iterm2 => "Iterm2",
            Self::Halfblocks => "Halfblocks",
        }
    }
}
