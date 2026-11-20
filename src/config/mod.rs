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

/// The classification/flagged state for an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Classification {
    /// Image has not been flagged (`⚪`).
    #[default]
    Unflagged,
    /// Image is marked as a keeper/favorite (`⭐`).
    Pick,
    /// Image is marked for rejection/deletion (`❌`).
    Reject,
}

impl Classification {
    /// Returns the emoji icon representing the classification.
    pub fn icon(&self) -> &'static str {
        match self {
            Self::Unflagged => "⚪",
            Self::Pick => "⭐",
            Self::Reject => "❌",
        }
    }

    /// Returns the user-facing display label with emoji.
    pub fn display_label(&self) -> &'static str {
        match self {
            Self::Unflagged => "⚪ Unflagged",
            Self::Pick => "⭐ Pick",
            Self::Reject => "❌ Reject",
        }
    }

    /// Returns the leading prefix used in search/palette listings.
    pub fn search_prefix(&self) -> &'static str {
        match self {
            Self::Unflagged => "   ",
            Self::Pick => "⭐ ",
            Self::Reject => "❌ ",
        }
    }

    /// Returns the uppercase text representation for export/serialization.
    pub fn export_prefix(&self) -> &'static str {
        match self {
            Self::Pick => "PICK",
            Self::Reject => "REJECT",
            Self::Unflagged => "UNFLAGGED",
        }
    }
}

/// Active filtering view mode controlling which classified images are visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ViewMode {
    /// Default view mode showing unflagged images and picks.
    #[default]
    Default,
    /// Show unflagged images only.
    Unflagged,
    /// Show picks only.
    Picks,
    /// Show rejected images only.
    Rejects,
    /// Show all files regardless of classification.
    All,
}

impl ViewMode {
    /// Returns the user-facing name for this view mode.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Default => "Unflagged + Picks",
            Self::Unflagged => "Unflagged Only",
            Self::Picks => "Picks Only",
            Self::Rejects => "Rejects Only",
            Self::All => "All Files",
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

/// Controls whether terminal graphics payloads use transport compression
/// (zlib `o=z` deflate for Kitty, or quality-90 JPEG for opaque iTerm2 images).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompressionPolicy {
    /// Automatically enable compression only when running over a remote SSH session
    /// (`$SSH_CONNECTION`, `$SSH_CLIENT`, or `$SSH_TTY`).
    #[default]
    Auto,
    /// Always compress graphics payloads.
    Always,
    /// Never compress graphics payloads (fastest lossless mode on local PTYs).
    Never,
}

impl CompressionPolicy {
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

    /// Determines whether graphics payloads should be compressed in the current environment.
    pub fn should_compress(self) -> bool {
        self.should_compress_for_env(Self::is_remote_ssh_session())
    }
}

/// Validated CLI protocol configuration encoding protocol-specific options in the type system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolConfig {
    /// Auto-detect terminal protocol; if `--compress` was explicitly set,
    /// `explicit_compress` is `Some(...)` and requires the detected protocol to be Kitty or iTerm2.
    Auto {
        /// Explicit `--compress` (or `--kitty-compress`) override passed on the CLI, if any.
        explicit_compress: Option<CompressionPolicy>,
        /// Explicit `--no-kitty-shm` flag passed on the CLI.
        disable_kitty_shm: bool,
    },
    /// Force Kitty graphics protocol with the specified compression policy.
    Kitty {
        /// Compression policy (`Auto` when `--compress` was omitted).
        compression: CompressionPolicy,
        /// Explicit `--no-kitty-shm` flag passed on the CLI.
        disable_kitty_shm: bool,
    },
    /// Force iTerm2 inline image protocol with the specified compression policy.
    Iterm2 {
        /// Compression policy (`Auto` when `--compress` was omitted).
        compression: CompressionPolicy,
    },
    /// Force DEC Sixel graphics protocol.
    Sixel,
    /// Force Unicode halfblocks fallback.
    Halfblocks,
}

impl Default for ProtocolConfig {
    fn default() -> Self {
        Self::Auto {
            explicit_compress: None,
            disable_kitty_shm: false,
        }
    }
}

impl ProtocolConfig {
    /// Parses protocol configuration from CLI arguments, validating that protocol-specific options
    /// like `--compress` (Kitty/iTerm2) and `--no-kitty-shm` (Kitty only) match the selected protocol.
    pub fn from_cli(
        protocol: Option<ratatui_image::picker::ProtocolType>,
        explicit_compress: Option<CompressionPolicy>,
        disable_kitty_shm: bool,
    ) -> Result<Self, String> {
        if explicit_compress.is_some()
            && protocol.is_some_and(|p| {
                !matches!(
                    p,
                    ratatui_image::picker::ProtocolType::Kitty
                        | ratatui_image::picker::ProtocolType::Iterm2
                )
            })
        {
            return Err(
                "Error: Option --compress can only be used with the kitty or iterm2 protocol"
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
                explicit_compress,
                disable_kitty_shm,
            }),
            Some(ratatui_image::picker::ProtocolType::Kitty) => Ok(Self::Kitty {
                compression: explicit_compress.unwrap_or(CompressionPolicy::Auto),
                disable_kitty_shm,
            }),
            Some(ratatui_image::picker::ProtocolType::Iterm2) => Ok(Self::Iterm2 {
                compression: explicit_compress.unwrap_or(CompressionPolicy::Auto),
            }),
            Some(ratatui_image::picker::ProtocolType::Sixel) => Ok(Self::Sixel),
            Some(ratatui_image::picker::ProtocolType::Halfblocks) => Ok(Self::Halfblocks),
        }
    }

    /// Evaluates whether payload compression should be enabled for the configured protocol.
    pub fn should_enable_compression(self) -> bool {
        match self {
            Self::Kitty { compression, .. } | Self::Iterm2 { compression } => {
                compression.should_compress()
            }
            Self::Auto {
                explicit_compress, ..
            } => explicit_compress
                .unwrap_or(CompressionPolicy::Auto)
                .should_compress(),
            Self::Sixel | Self::Halfblocks => false,
        }
    }

    /// Evaluates whether Kitty zlib compression should be requested during terminal initialization.
    pub fn should_enable_kitty_compression(self) -> bool {
        match self {
            Self::Kitty { compression, .. } => compression.should_compress(),
            Self::Auto {
                explicit_compress, ..
            } => explicit_compress
                .unwrap_or(CompressionPolicy::Auto)
                .should_compress(),
            Self::Sixel | Self::Iterm2 { .. } | Self::Halfblocks => false,
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
            Self::Iterm2 { .. } => Some(ratatui_image::picker::ProtocolType::Iterm2),
            Self::Halfblocks => Some(ratatui_image::picker::ProtocolType::Halfblocks),
        }
    }

    /// Resolves the final `ActiveProtocol` based on the detected terminal protocol and compression state.
    pub fn resolve_active_protocol(
        self,
        detected: ratatui_image::picker::ProtocolType,
        compressed: bool,
    ) -> Result<ActiveProtocol, String> {
        match self {
            Self::Auto {
                explicit_compress,
                disable_kitty_shm,
            } => {
                if explicit_compress.is_some()
                    && !matches!(
                        detected,
                        ratatui_image::picker::ProtocolType::Kitty
                            | ratatui_image::picker::ProtocolType::Iterm2
                    )
                {
                    return Err(format!(
                        "Error: Option --compress was specified, but detected terminal protocol is {:?} (not Kitty or Iterm2)",
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
                        let mode = if compressed {
                            KittyTransferMode::Compressed
                        } else {
                            KittyTransferMode::Direct
                        };
                        Ok(ActiveProtocol::Kitty(mode))
                    }
                    ratatui_image::picker::ProtocolType::Iterm2 => {
                        let mode = if compressed {
                            Iterm2TransferMode::Jpeg
                        } else {
                            Iterm2TransferMode::Png
                        };
                        Ok(ActiveProtocol::Iterm2(mode))
                    }
                    ratatui_image::picker::ProtocolType::Sixel => Ok(ActiveProtocol::Sixel),
                    ratatui_image::picker::ProtocolType::Halfblocks => {
                        Ok(ActiveProtocol::Halfblocks)
                    }
                }
            }
            Self::Kitty { .. } => {
                let mode = if compressed {
                    KittyTransferMode::Compressed
                } else {
                    KittyTransferMode::Direct
                };
                Ok(ActiveProtocol::Kitty(mode))
            }
            Self::Iterm2 { .. } => {
                let mode = if compressed {
                    Iterm2TransferMode::Jpeg
                } else {
                    Iterm2TransferMode::Png
                };
                Ok(ActiveProtocol::Iterm2(mode))
            }
            Self::Sixel => Ok(ActiveProtocol::Sixel),
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

/// Transmission encoding mode for the iTerm2 inline image protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Iterm2TransferMode {
    /// Lossless PNG (`Fast` + `Paeth`) inline transmission.
    #[default]
    Png,
    /// Quality-90 JPEG inline transmission for opaque images (falls back to PNG when transparent).
    Jpeg,
}

/// Active runtime terminal graphics protocol and its protocol-specific configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActiveProtocol {
    /// Kitty graphics protocol with its transmission mode.
    Kitty(KittyTransferMode),
    /// DEC Sixel graphics protocol.
    Sixel,
    /// iTerm2 inline images protocol with its encoding mode.
    Iterm2(Iterm2TransferMode),
    /// Unicode halfblock character fallback.
    #[default]
    Halfblocks,
}

impl ActiveProtocol {
    #[cfg(test)]
    /// Constructs an `ActiveProtocol` from a `ProtocolType` and a compression state.
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
            ratatui_image::picker::ProtocolType::Iterm2 => {
                if compressed {
                    Self::Iterm2(Iterm2TransferMode::Jpeg)
                } else {
                    Self::Iterm2(Iterm2TransferMode::Png)
                }
            }
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

    /// Returns `true` if Kitty zlib transport compression (`o=z`) is active.
    pub const fn uses_kitty_compression(self) -> bool {
        matches!(self, Self::Kitty(KittyTransferMode::Compressed))
    }

    /// Returns `true` if iTerm2 JPEG compression is active for opaque images.
    pub const fn uses_iterm2_jpeg(self) -> bool {
        matches!(self, Self::Iterm2(Iterm2TransferMode::Jpeg))
    }

    /// Returns the descriptive label for Stats for Nerds, indicating compression or SHM only when active.
    pub const fn display_label(self) -> &'static str {
        match self {
            Self::Kitty(KittyTransferMode::Direct) => "Kitty",
            Self::Kitty(KittyTransferMode::Compressed) => "Kitty (Compressed)",
            Self::Kitty(KittyTransferMode::SharedMemory) => "Kitty (SHM)",
            Self::Sixel => "Sixel",
            Self::Iterm2(Iterm2TransferMode::Png) => "Iterm2",
            Self::Iterm2(Iterm2TransferMode::Jpeg) => "Iterm2 (JPEG)",
            Self::Halfblocks => "Halfblocks",
        }
    }
}
