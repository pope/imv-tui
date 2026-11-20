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
