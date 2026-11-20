//! Unified viewport graphics protocol abstraction.
//!
//! Dispatches rendering across standard `ratatui-image` stateful protocols,
//! zero-copy POSIX shared memory Kitty (`t=s`), parallel `rayon` inline Kitty (`t=d` / `o=z,t=d`),
//! parallel `rayon` Sixel, fast PNG/JPEG iTerm2 (`OSC 1337`), and SIMD Unicode halfblocks.

use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui_image::ResizeEncodeRender;
use ratatui_image::protocol::StatefulProtocol;

use super::halfblocks::HalfblocksProtocol;
use super::iterm2::Iterm2Protocol;
use super::kitty_direct::KittyDirectProtocol;
use super::kitty_shm::KittyShmProtocol;
use super::sixel::SixelProtocol;

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

/// Palette compression mode for the DEC Sixel graphics protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SixelTransferMode {
    /// Full 256-color palette with 4x4 Bayer ordered dithering.
    #[default]
    Full,
    /// Reduced 128-color palette with 4x4 Bayer ordered dithering for smaller payloads.
    Compressed,
}

/// Active runtime terminal graphics protocol and its protocol-specific configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActiveProtocol {
    /// Kitty graphics protocol with its transmission mode.
    Kitty(KittyTransferMode),
    /// DEC Sixel graphics protocol with its palette compression mode.
    Sixel(SixelTransferMode),
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
            ratatui_image::picker::ProtocolType::Sixel => {
                if compressed {
                    Self::Sixel(SixelTransferMode::Compressed)
                } else {
                    Self::Sixel(SixelTransferMode::Full)
                }
            }
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

    /// Returns the descriptive label for Stats for Nerds, indicating compression or SHM only when active.
    pub const fn display_label(self) -> &'static str {
        match self {
            Self::Kitty(KittyTransferMode::Direct) => "Kitty",
            Self::Kitty(KittyTransferMode::Compressed) => "Kitty (Compressed)",
            Self::Kitty(KittyTransferMode::SharedMemory) => "Kitty (SHM)",
            Self::Sixel(SixelTransferMode::Full) => "Sixel",
            Self::Sixel(SixelTransferMode::Compressed) => "Sixel (Compressed)",
            Self::Iterm2(Iterm2TransferMode::Png) => "Iterm2",
            Self::Iterm2(Iterm2TransferMode::Jpeg) => "Iterm2 (JPEG)",
            Self::Halfblocks => "Halfblocks",
        }
    }
}

/// Unified viewport image protocol supporting standard `ratatui-image` protocols,
/// the zero-copy POSIX shared memory (`t=s`) Kitty protocol, the parallel inline (`t=d` / `o=z,t=d`)
/// Kitty protocol, the parallel Sixel protocol, the fast PNG/JPEG iTerm2 (`OSC 1337`) protocol,
/// and the SIMD Unicode halfblocks protocol.
pub enum ViewportProtocol {
    /// Standard `ratatui-image` stateful fallback protocol.
    Standard(StatefulProtocol),
    /// Zero-copy POSIX shared memory Kitty protocol (`t=s`).
    KittyShm(KittyShmProtocol),
    /// Parallel `rayon`-accelerated inline Kitty protocol (`t=d` / `o=z,t=d`).
    KittyDirect(KittyDirectProtocol),
    /// Parallel `rayon`-accelerated Sixel protocol.
    Sixel(SixelProtocol),
    /// Fast PNG (`Fast` + `Paeth`) / JPEG (q90) iTerm2 inline image protocol (`OSC 1337`).
    Iterm2(Iterm2Protocol),
    /// Fast SIMD-resized Unicode halfblocks protocol.
    Halfblocks(HalfblocksProtocol),
}

impl ResizeEncodeRender for ViewportProtocol {
    fn resize_encode(&mut self, resize: &ratatui_image::Resize, size: Size) {
        match self {
            Self::Standard(proto) => proto.resize_encode(resize, size),
            Self::KittyShm(_)
            | Self::KittyDirect(_)
            | Self::Sixel(_)
            | Self::Iterm2(_)
            | Self::Halfblocks(_) => {}
        }
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer) {
        match self {
            Self::Standard(proto) => proto.render(area, buf),
            Self::KittyShm(proto) => proto.render(area, buf),
            Self::KittyDirect(proto) => proto.render(area, buf),
            Self::Sixel(proto) => proto.render(area, buf),
            Self::Iterm2(proto) => proto.render(area, buf),
            Self::Halfblocks(proto) => proto.render(area, buf),
        }
    }

    fn needs_resize(&self, resize: &ratatui_image::Resize, size: Size) -> Option<Size> {
        match self {
            Self::Standard(proto) => proto.needs_resize(resize, size),
            Self::KittyShm(_)
            | Self::KittyDirect(_)
            | Self::Sixel(_)
            | Self::Iterm2(_)
            | Self::Halfblocks(_) => None,
        }
    }
}
