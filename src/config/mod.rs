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
        /// Explicit `--no-kitty-shm` flag passed on the CLI.
        disable_kitty_shm: bool,
    },
    /// Force Kitty graphics protocol with the specified compression policy.
    Kitty {
        /// Compression policy (`Auto` when `--kitty-compress` was omitted).
        compression: KittyCompression,
        /// Explicit `--no-kitty-shm` flag passed on the CLI.
        disable_kitty_shm: bool,
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
            disable_kitty_shm: false,
        }
    }
}

impl ProtocolConfig {
    /// Parses protocol configuration from CLI arguments, validating that protocol-specific options
    /// like `--kitty-compress` and `--no-kitty-shm` are only used with Kitty (or auto-detection).
    pub fn from_cli(
        protocol: Option<ratatui_image::picker::ProtocolType>,
        explicit_kitty_compress: Option<KittyCompression>,
        disable_kitty_shm: bool,
    ) -> Result<Self, String> {
        if explicit_kitty_compress.is_some()
            && protocol.is_some_and(|p| p != ratatui_image::picker::ProtocolType::Kitty)
        {
            return Err(
                "Error: Option --kitty-compress can only be used with the kitty protocol"
                    .to_string(),
            );
        }
        if disable_kitty_shm
            && protocol.is_some_and(|p| p != ratatui_image::picker::ProtocolType::Kitty)
        {
            return Err(
                "Error: Option --no-kitty-shm can only be used with the kitty protocol".to_string(),
            );
        }
        match protocol {
            None => Ok(Self::Auto {
                explicit_kitty_compress,
                disable_kitty_shm,
            }),
            Some(ratatui_image::picker::ProtocolType::Kitty) => Ok(Self::Kitty {
                compression: explicit_kitty_compress.unwrap_or(KittyCompression::Auto),
                disable_kitty_shm,
            }),
            Some(ratatui_image::picker::ProtocolType::Sixel) => Ok(Self::Sixel),
            Some(ratatui_image::picker::ProtocolType::Iterm2) => Ok(Self::Iterm2),
            Some(ratatui_image::picker::ProtocolType::Halfblocks) => Ok(Self::Halfblocks),
        }
    }

    /// Evaluates whether Kitty compression should be requested during terminal initialization.
    pub fn should_enable_kitty_compression(self) -> bool {
        match self {
            Self::Kitty { compression, .. } => compression.should_compress(),
            Self::Auto {
                explicit_kitty_compress,
                ..
            } => explicit_kitty_compress
                .unwrap_or(KittyCompression::Auto)
                .should_compress(),
            Self::Sixel | Self::Iterm2 | Self::Halfblocks => false,
        }
    }

    /// Returns whether Kitty POSIX shared memory support probing should be performed on startup.
    pub const fn should_probe_kitty_shm(self) -> bool {
        matches!(
            self,
            Self::Auto {
                disable_kitty_shm: false,
                ..
            } | Self::Kitty {
                disable_kitty_shm: false,
                ..
            }
        )
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
                disable_kitty_shm,
            } => {
                if explicit_kitty_compress.is_some()
                    && detected != ratatui_image::picker::ProtocolType::Kitty
                {
                    return Err(format!(
                        "Error: Option --kitty-compress was specified, but detected terminal protocol is {:?} (not Kitty)",
                        detected
                    ));
                }
                if disable_kitty_shm && detected != ratatui_image::picker::ProtocolType::Kitty {
                    return Err(format!(
                        "Error: Option --no-kitty-shm was specified, but detected terminal protocol is {:?} (not Kitty)",
                        detected
                    ));
                }
                match detected {
                    ratatui_image::picker::ProtocolType::Kitty => {
                        let mode = if kitty_compressed {
                            KittyTransferMode::Compressed
                        } else {
                            KittyTransferMode::Direct
                        };
                        Ok(ActiveProtocol::Kitty(mode))
                    }
                    ratatui_image::picker::ProtocolType::Sixel => Ok(ActiveProtocol::Sixel),
                    ratatui_image::picker::ProtocolType::Iterm2 => Ok(ActiveProtocol::Iterm2),
                    ratatui_image::picker::ProtocolType::Halfblocks => {
                        Ok(ActiveProtocol::Halfblocks)
                    }
                }
            }
            Self::Kitty { .. } => {
                let mode = if kitty_compressed {
                    KittyTransferMode::Compressed
                } else {
                    KittyTransferMode::Direct
                };
                Ok(ActiveProtocol::Kitty(mode))
            }
            Self::Sixel => Ok(ActiveProtocol::Sixel),
            Self::Iterm2 => Ok(ActiveProtocol::Iterm2),
            Self::Halfblocks => Ok(ActiveProtocol::Halfblocks),
        }
    }
}

/// Transmission mode for the Kitty graphics protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KittyTransferMode {
    /// Standard uncompressed inline Base64 transmission (`t=d`).
    #[default]
    Direct,
    /// Zlib-compressed inline Base64 transmission (`o=z,t=d`).
    Compressed,
    /// Zero-copy POSIX shared memory transmission (`t=s`).
    SharedMemory,
}

/// Active runtime terminal graphics protocol and its protocol-specific configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActiveProtocol {
    /// Kitty graphics protocol with its transmission mode.
    Kitty(KittyTransferMode),
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
            ratatui_image::picker::ProtocolType::Kitty => {
                if compressed {
                    Self::Kitty(KittyTransferMode::Compressed)
                } else {
                    Self::Kitty(KittyTransferMode::Direct)
                }
            }
            ratatui_image::picker::ProtocolType::Sixel => Self::Sixel,
            ratatui_image::picker::ProtocolType::Iterm2 => Self::Iterm2,
            ratatui_image::picker::ProtocolType::Halfblocks => Self::Halfblocks,
        }
    }

    /// Returns an updated `ActiveProtocol` enabling or disabling Kitty shared memory transmission.
    ///
    /// Only upgrades `Self::Kitty(KittyTransferMode::Direct)` to `Self::Kitty(KittyTransferMode::SharedMemory)`
    /// when `shm_supported` is `true` (never touching `Compressed`).
    pub const fn with_kitty_shm(self, shm_supported: bool) -> Self {
        match self {
            Self::Kitty(KittyTransferMode::Direct) if shm_supported => {
                Self::Kitty(KittyTransferMode::SharedMemory)
            }
            Self::Kitty(KittyTransferMode::SharedMemory) if !shm_supported => {
                Self::Kitty(KittyTransferMode::Direct)
            }
            other => other,
        }
    }

    /// Returns `true` if POSIX shared memory transmission is active.
    pub const fn uses_kitty_shm(self) -> bool {
        matches!(self, Self::Kitty(KittyTransferMode::SharedMemory))
    }

    /// Returns the descriptive label for Stats for Nerds, indicating compression or SHM only when active.
    pub const fn display_label(self) -> &'static str {
        match self {
            Self::Kitty(KittyTransferMode::Direct) => "Kitty",
            Self::Kitty(KittyTransferMode::Compressed) => "Kitty (Compressed)",
            Self::Kitty(KittyTransferMode::SharedMemory) => "Kitty (SHM)",
            Self::Sixel => "Sixel",
            Self::Iterm2 => "Iterm2",
            Self::Halfblocks => "Halfblocks",
        }
    }
}
