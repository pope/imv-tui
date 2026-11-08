/// Drawing view logic for standard and protocol images.
pub mod image_view;
/// Overlay panel presenting metadata information list.
pub mod info_box;
/// Top/bottom status bar presenting file subpaths and settings.
pub mod infobar;
/// Overlay input text search panel for commands and files.
pub mod palette;
/// Centered floating prompt input box.
pub mod prompt;
/// Small floating slideshow paused notification indicator.
pub mod slideshow_paused;

use crate::config::InfoBarPosition;
use ratatui::layout::Rect;

/// Computes a bounded top-right overlay `Rect` inside `area`, respecting the infobar position.
pub fn top_right_popup_rect(
    area: Rect,
    desired_w: u16,
    desired_h: u16,
    infobar: InfoBarPosition,
) -> Rect {
    let w = desired_w.min(area.width.saturating_sub(1)).max(1);
    let h = desired_h.min(area.height.saturating_sub(1)).max(1);
    let x = area.x + area.width.saturating_sub(w).saturating_sub(1);
    let y = if infobar == InfoBarPosition::Top {
        area.y
    } else {
        area.y.saturating_add(1)
    };
    let x = x.min(area.right().saturating_sub(w));
    let y = y.min(area.bottom().saturating_sub(h));
    Rect::new(x, y, w, h)
}
