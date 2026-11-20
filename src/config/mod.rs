/// Command-line argument parsing.
pub mod cli;
/// Key definitions and crossterm matching logic.
pub mod keys;

pub use crate::imaging::{
    ActiveProtocol, Iterm2TransferMode, KittyTransferMode, SixelTransferMode,
};

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
/// (zlib `o=z` deflate for Kitty, quality-90 JPEG for opaque iTerm2 images,
/// or 128-color palette quantization for Sixel).
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
    /// `explicit_compress` is `Some(...)` and requires the detected protocol to be Kitty, iTerm2, or Sixel.
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
    /// Force DEC Sixel graphics protocol with the specified compression policy.
    Sixel {
        /// Compression policy (`Auto` when `--compress` was omitted).
        compression: CompressionPolicy,
    },
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
    /// like `--compress` (Kitty/iTerm2/Sixel) and `--no-kitty-shm` (Kitty only) match the selected protocol.
    pub fn from_cli(
        protocol: Option<crate::imaging::ProtocolType>,
        explicit_compress: Option<CompressionPolicy>,
        disable_kitty_shm: bool,
    ) -> Result<Self, String> {
        if explicit_compress.is_some()
            && protocol.is_some_and(|p| {
                !matches!(
                    p,
                    crate::imaging::ProtocolType::Kitty
                        | crate::imaging::ProtocolType::Iterm2
                        | crate::imaging::ProtocolType::Sixel
                )
            })
        {
            return Err(
                "Error: Option --compress can only be used with the kitty, iterm2, or sixel protocol"
                    .to_string(),
            );
        }
        if disable_kitty_shm && protocol.is_some_and(|p| p != crate::imaging::ProtocolType::Kitty) {
            return Err(
                "Error: Option --no-kitty-shm can only be used with the kitty protocol".to_string(),
            );
        }
        match protocol {
            None => Ok(Self::Auto {
                explicit_compress,
                disable_kitty_shm,
            }),
            Some(crate::imaging::ProtocolType::Kitty) => Ok(Self::Kitty {
                compression: explicit_compress.unwrap_or(CompressionPolicy::Auto),
                disable_kitty_shm,
            }),
            Some(crate::imaging::ProtocolType::Iterm2) => Ok(Self::Iterm2 {
                compression: explicit_compress.unwrap_or(CompressionPolicy::Auto),
            }),
            Some(crate::imaging::ProtocolType::Sixel) => Ok(Self::Sixel {
                compression: explicit_compress.unwrap_or(CompressionPolicy::Auto),
            }),
            Some(crate::imaging::ProtocolType::Halfblocks) => Ok(Self::Halfblocks),
        }
    }

    /// Evaluates whether payload compression should be enabled for the configured protocol.
    pub fn should_enable_compression(self) -> bool {
        match self {
            Self::Kitty { compression, .. }
            | Self::Iterm2 { compression }
            | Self::Sixel { compression } => compression.should_compress(),
            Self::Auto {
                explicit_compress, ..
            } => explicit_compress
                .unwrap_or(CompressionPolicy::Auto)
                .should_compress(),
            Self::Halfblocks => false,
        }
    }

    /// Evaluates whether payload compression should be enabled for `effective_protocol`,
    /// taking into account whether the terminal confirmed Kitty `o=z` zlib support.
    ///
    /// For Kitty under [`CompressionPolicy::Auto`] (whether `--protocol kitty` was forced or
    /// auto-detected), compression is gated on `supports_kitty_compression` so terminals that
    /// reject `o=z` are not sent compressed payloads. Explicit [`CompressionPolicy::Always`]
    /// always honors `true`, and [`CompressionPolicy::Never`] always returns `false`.
    pub fn effective_compression(
        self,
        effective_protocol: crate::imaging::ProtocolType,
        supports_kitty_compression: bool,
    ) -> bool {
        let compression_enabled = self.should_enable_compression();
        let is_auto = match self {
            Self::Kitty { compression, .. }
            | Self::Iterm2 { compression }
            | Self::Sixel { compression } => compression == CompressionPolicy::Auto,
            Self::Auto {
                explicit_compress, ..
            } => explicit_compress.unwrap_or(CompressionPolicy::Auto) == CompressionPolicy::Auto,
            Self::Halfblocks => false,
        };
        if effective_protocol == crate::imaging::ProtocolType::Kitty && is_auto {
            compression_enabled && supports_kitty_compression
        } else {
            compression_enabled
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
            Self::Sixel { .. } | Self::Iterm2 { .. } | Self::Halfblocks => false,
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
    pub fn forced_protocol_type(self) -> Option<crate::imaging::ProtocolType> {
        match self {
            Self::Auto { .. } => None,
            Self::Kitty { .. } => Some(crate::imaging::ProtocolType::Kitty),
            Self::Sixel { .. } => Some(crate::imaging::ProtocolType::Sixel),
            Self::Iterm2 { .. } => Some(crate::imaging::ProtocolType::Iterm2),
            Self::Halfblocks => Some(crate::imaging::ProtocolType::Halfblocks),
        }
    }

    /// Resolves the final `ActiveProtocol` based on the detected terminal protocol and compression state.
    pub fn resolve_active_protocol(
        self,
        detected: crate::imaging::ProtocolType,
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
                        crate::imaging::ProtocolType::Kitty
                            | crate::imaging::ProtocolType::Iterm2
                            | crate::imaging::ProtocolType::Sixel
                    )
                {
                    return Err(format!(
                        "Error: Option --compress was specified, but detected terminal protocol is {:?} (not Kitty, Iterm2, or Sixel)",
                        detected
                    ));
                }
                if disable_kitty_shm && detected != crate::imaging::ProtocolType::Kitty {
                    return Err(format!(
                        "Error: Option --no-kitty-shm was specified, but detected terminal protocol is {:?} (not Kitty)",
                        detected
                    ));
                }
                match detected {
                    crate::imaging::ProtocolType::Kitty => {
                        let mode = if compressed {
                            KittyTransferMode::Compressed
                        } else {
                            KittyTransferMode::Direct
                        };
                        Ok(ActiveProtocol::Kitty(mode))
                    }
                    crate::imaging::ProtocolType::Iterm2 => {
                        let mode = if compressed {
                            Iterm2TransferMode::Jpeg
                        } else {
                            Iterm2TransferMode::Png
                        };
                        Ok(ActiveProtocol::Iterm2(mode))
                    }
                    crate::imaging::ProtocolType::Sixel => {
                        let mode = if compressed {
                            SixelTransferMode::Compressed
                        } else {
                            SixelTransferMode::Full
                        };
                        Ok(ActiveProtocol::Sixel(mode))
                    }
                    crate::imaging::ProtocolType::Halfblocks => Ok(ActiveProtocol::Halfblocks),
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
            Self::Sixel { .. } => {
                let mode = if compressed {
                    SixelTransferMode::Compressed
                } else {
                    SixelTransferMode::Full
                };
                Ok(ActiveProtocol::Sixel(mode))
            }
            Self::Halfblocks => Ok(ActiveProtocol::Halfblocks),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imaging::ProtocolType;

    #[test]
    fn test_effective_compression_kitty_policy_gating() {
        // Explicit Always honors true regardless of Kitty probe result.
        let forced_always = ProtocolConfig::Kitty {
            compression: CompressionPolicy::Always,
            disable_kitty_shm: false,
        };
        assert!(forced_always.effective_compression(ProtocolType::Kitty, false));
        assert!(forced_always.effective_compression(ProtocolType::Kitty, true));

        let auto_always = ProtocolConfig::Auto {
            explicit_compress: Some(CompressionPolicy::Always),
            disable_kitty_shm: false,
        };
        assert!(auto_always.effective_compression(ProtocolType::Kitty, false));
        assert!(auto_always.effective_compression(ProtocolType::Kitty, true));

        // Explicit Never honors false regardless of Kitty probe result.
        let forced_never = ProtocolConfig::Kitty {
            compression: CompressionPolicy::Never,
            disable_kitty_shm: false,
        };
        assert!(!forced_never.effective_compression(ProtocolType::Kitty, true));
        assert!(!forced_never.effective_compression(ProtocolType::Kitty, false));

        // Auto policy gates Kitty on supports_kitty_compression (both forced and auto protocol).
        let forced_auto = ProtocolConfig::Kitty {
            compression: CompressionPolicy::Auto,
            disable_kitty_shm: false,
        };
        assert!(!forced_auto.effective_compression(ProtocolType::Kitty, false));
        assert_eq!(
            forced_auto.effective_compression(ProtocolType::Kitty, true),
            CompressionPolicy::Auto.should_compress()
        );

        let auto_proto_auto = ProtocolConfig::Auto {
            explicit_compress: None,
            disable_kitty_shm: false,
        };
        assert!(!auto_proto_auto.effective_compression(ProtocolType::Kitty, false));
        assert_eq!(
            auto_proto_auto.effective_compression(ProtocolType::Kitty, true),
            CompressionPolicy::Auto.should_compress()
        );

        // Halfblocks always disables compression.
        assert!(!ProtocolConfig::Halfblocks.effective_compression(ProtocolType::Halfblocks, true));
    }
}
