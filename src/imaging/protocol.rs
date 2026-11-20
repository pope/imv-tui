//! Unified viewport graphics protocol abstraction.
//!
//! Dispatches rendering across standard `ratatui-image` stateful protocols,
//! zero-copy POSIX shared memory Kitty (`t=s`), and parallel `rayon` Sixel.

use ratatui::buffer::Buffer;
use ratatui::layout::{Rect, Size};
use ratatui_image::ResizeEncodeRender;
use ratatui_image::protocol::StatefulProtocol;

use super::kitty_shm::KittyShmProtocol;
use super::sixel::SixelProtocol;

/// Unified viewport image protocol supporting standard `ratatui-image` protocols,
/// the zero-copy POSIX shared memory (`t=s`) Kitty protocol, and the parallel Sixel protocol.
pub enum ViewportProtocol {
    /// Standard `ratatui-image` stateful protocol (`iTerm2`, `Halfblocks`, or `Kitty` `t=d`).
    Standard(StatefulProtocol),
    /// Zero-copy POSIX shared memory Kitty protocol (`t=s`).
    KittyShm(KittyShmProtocol),
    /// Parallel `rayon`-accelerated Sixel protocol.
    Sixel(SixelProtocol),
}

impl ResizeEncodeRender for ViewportProtocol {
    fn resize_encode(&mut self, resize: &ratatui_image::Resize, size: Size) {
        match self {
            Self::Standard(proto) => proto.resize_encode(resize, size),
            Self::KittyShm(_) | Self::Sixel(_) => {}
        }
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer) {
        match self {
            Self::Standard(proto) => proto.render(area, buf),
            Self::KittyShm(proto) => proto.render(area, buf),
            Self::Sixel(proto) => proto.render(area, buf),
        }
    }

    fn needs_resize(&self, resize: &ratatui_image::Resize, size: Size) -> Option<Size> {
        match self {
            Self::Standard(proto) => proto.needs_resize(resize, size),
            Self::KittyShm(_) | Self::Sixel(_) => None,
        }
    }
}
