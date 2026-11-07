//! Zero-copy POSIX shared memory (`t=s`) Kitty graphics protocol implementation.
//!
//! Provides high-throughput direct frame transmission via `/dev/shm` shared memory objects,
//! eliminating base64 encoding and terminal pipe bandwidth bottlenecks.

use std::collections::VecDeque;
use std::ffi::CString;
use std::fs::File;
use std::io::Write;
use std::num::NonZeroU16;
use std::os::fd::FromRawFd;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use image::DynamicImage;
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::{Rect, Size};
use ratatui::style::Color;
use ratatui_image::ResizeEncodeRender;
use ratatui_image::protocol::StatefulProtocol;

/// Unified viewport image protocol supporting both standard `ratatui-image` protocols
/// and the zero-copy POSIX shared memory (`t=s`) Kitty protocol.
pub enum ViewportProtocol {
    /// Standard `ratatui-image` stateful protocol (`Sixel`, `iTerm2`, `Halfblocks`, or `Kitty` `t=d`).
    Standard(StatefulProtocol),
    /// Zero-copy POSIX shared memory Kitty protocol (`t=s`).
    KittyShm(KittyShmProtocol),
}

impl ResizeEncodeRender for ViewportProtocol {
    fn resize_encode(&mut self, resize: &ratatui_image::Resize, size: Size) {
        match self {
            Self::Standard(proto) => proto.resize_encode(resize, size),
            Self::KittyShm(_) => {}
        }
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer) {
        match self {
            Self::Standard(proto) => proto.render(area, buf),
            Self::KittyShm(proto) => proto.render(area, buf),
        }
    }

    fn needs_resize(&self, resize: &ratatui_image::Resize, size: Size) -> Option<Size> {
        match self {
            Self::Standard(proto) => proto.needs_resize(resize, size),
            Self::KittyShm(_) => None,
        }
    }
}

/// Standard RFC 4648 Base64 encoding for ASCII byte sequences.
pub fn encode_base64_ascii(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);

        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);

        if chunk.len() > 1 {
            out.push(TABLE[(((b1 & 0x0F) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }

        if chunk.len() > 2 {
            out.push(TABLE[(b2 & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

static TRANSMITTED_SHM_RING: Mutex<VecDeque<CString>> = Mutex::new(VecDeque::new());
static SHM_SEQ: AtomicU64 = AtomicU64::new(1);
static NEXT_KITTY_ID: AtomicU32 = AtomicU32::new(100);

fn next_kitty_id() -> u32 {
    let id = NEXT_KITTY_ID.fetch_add(1, Ordering::Relaxed);
    if id >= 0x00FF_FFFE {
        NEXT_KITTY_ID.store(100, Ordering::Relaxed);
    }
    id
}

/// Drains and unlinks all retained POSIX shared memory segments from `/dev/shm`.
pub fn cleanup_all_kitty_shm_segments() {
    if let Ok(mut ring) = TRANSMITTED_SHM_RING.lock() {
        while let Some(oldest) = ring.pop_front() {
            unsafe {
                libc::shm_unlink(oldest.as_ptr());
            }
        }
    }
}

/// A POSIX shared memory segment holding raw RGBA pixel data for Kitty transmission.
pub struct KittyShmSegment {
    c_name: CString,
    shm_name: String,
    transmitted: AtomicBool,
}

impl KittyShmSegment {
    /// Creates a new POSIX shared memory object and writes the raw pixel bytes into it.
    pub fn new(raw_bytes: &[u8]) -> Result<Self, String> {
        let seq = SHM_SEQ.fetch_add(1, Ordering::Relaxed);
        let shm_name = format!("/imv-tui-{}-{}", std::process::id(), seq);
        let c_name = CString::new(shm_name.clone())
            .map_err(|e| format!("Invalid CString for shm_name: {}", e))?;

        let fd = unsafe {
            libc::shm_open(
                c_name.as_ptr(),
                libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                0o600,
            )
        };
        if fd < 0 {
            return Err(format!(
                "shm_open failed for {}: {}",
                shm_name,
                std::io::Error::last_os_error()
            ));
        }

        let mut file = unsafe { File::from_raw_fd(fd) };
        if let Err(e) = file.set_len(raw_bytes.len() as u64) {
            unsafe {
                libc::shm_unlink(c_name.as_ptr());
            }
            return Err(format!("Failed to set shm size for {}: {}", shm_name, e));
        }
        if let Err(e) = file.write_all(raw_bytes) {
            unsafe {
                libc::shm_unlink(c_name.as_ptr());
            }
            return Err(format!("Failed to write bytes to shm {}: {}", shm_name, e));
        }
        drop(file);

        Ok(Self {
            c_name,
            shm_name,
            transmitted: AtomicBool::new(false),
        })
    }

    /// Returns the shared memory object name.
    pub fn shm_name(&self) -> &str {
        &self.shm_name
    }

    /// Returns `true` if the frame has been transmitted to the terminal.
    #[cfg(test)]
    pub fn is_transmitted(&self) -> bool {
        self.transmitted.load(Ordering::Acquire)
    }
}

impl Drop for KittyShmSegment {
    fn drop(&mut self) {
        if !self.transmitted.load(Ordering::Acquire) {
            unsafe {
                libc::shm_unlink(self.c_name.as_ptr());
            }
        }
    }
}

const PLACEHOLDER: char = '\u{10EEEE}';
const UNIT_WIDTH: CellDiffOption = CellDiffOption::ForcedWidth(NonZeroU16::new(1).unwrap());

/// 297-entry Kitty diacritics table for row/column/extra placement.
static DIACRITICS: [char; 297] = [
    '\u{305}',
    '\u{30D}',
    '\u{30E}',
    '\u{310}',
    '\u{312}',
    '\u{33D}',
    '\u{33E}',
    '\u{33F}',
    '\u{346}',
    '\u{34A}',
    '\u{34B}',
    '\u{34C}',
    '\u{350}',
    '\u{351}',
    '\u{352}',
    '\u{357}',
    '\u{35B}',
    '\u{363}',
    '\u{364}',
    '\u{365}',
    '\u{366}',
    '\u{367}',
    '\u{368}',
    '\u{369}',
    '\u{36A}',
    '\u{36B}',
    '\u{36C}',
    '\u{36D}',
    '\u{36E}',
    '\u{36F}',
    '\u{483}',
    '\u{484}',
    '\u{485}',
    '\u{486}',
    '\u{487}',
    '\u{592}',
    '\u{593}',
    '\u{594}',
    '\u{595}',
    '\u{597}',
    '\u{598}',
    '\u{599}',
    '\u{59C}',
    '\u{59D}',
    '\u{59E}',
    '\u{59F}',
    '\u{5A0}',
    '\u{5A1}',
    '\u{5A8}',
    '\u{5A9}',
    '\u{5AB}',
    '\u{5AC}',
    '\u{5AF}',
    '\u{5C4}',
    '\u{610}',
    '\u{611}',
    '\u{612}',
    '\u{613}',
    '\u{614}',
    '\u{615}',
    '\u{616}',
    '\u{617}',
    '\u{657}',
    '\u{658}',
    '\u{659}',
    '\u{65A}',
    '\u{65B}',
    '\u{65D}',
    '\u{65E}',
    '\u{6D6}',
    '\u{6D7}',
    '\u{6D8}',
    '\u{6D9}',
    '\u{6DA}',
    '\u{6DB}',
    '\u{6DC}',
    '\u{6DF}',
    '\u{6E0}',
    '\u{6E1}',
    '\u{6E2}',
    '\u{6E4}',
    '\u{6E7}',
    '\u{6E8}',
    '\u{6EB}',
    '\u{6EC}',
    '\u{730}',
    '\u{732}',
    '\u{733}',
    '\u{735}',
    '\u{736}',
    '\u{73A}',
    '\u{73D}',
    '\u{73F}',
    '\u{740}',
    '\u{741}',
    '\u{743}',
    '\u{745}',
    '\u{747}',
    '\u{749}',
    '\u{74A}',
    '\u{7EB}',
    '\u{7EC}',
    '\u{7ED}',
    '\u{7EE}',
    '\u{7EF}',
    '\u{7F0}',
    '\u{7F1}',
    '\u{7F3}',
    '\u{816}',
    '\u{817}',
    '\u{818}',
    '\u{819}',
    '\u{81B}',
    '\u{81C}',
    '\u{81D}',
    '\u{81E}',
    '\u{81F}',
    '\u{820}',
    '\u{821}',
    '\u{822}',
    '\u{823}',
    '\u{825}',
    '\u{826}',
    '\u{827}',
    '\u{829}',
    '\u{82A}',
    '\u{82B}',
    '\u{82C}',
    '\u{82D}',
    '\u{951}',
    '\u{953}',
    '\u{954}',
    '\u{F82}',
    '\u{F83}',
    '\u{F86}',
    '\u{F87}',
    '\u{135D}',
    '\u{135E}',
    '\u{135F}',
    '\u{17DD}',
    '\u{193A}',
    '\u{1A17}',
    '\u{1A75}',
    '\u{1A76}',
    '\u{1A77}',
    '\u{1A78}',
    '\u{1A79}',
    '\u{1A7A}',
    '\u{1A7B}',
    '\u{1A7C}',
    '\u{1B6B}',
    '\u{1B6D}',
    '\u{1B6E}',
    '\u{1B6F}',
    '\u{1B70}',
    '\u{1B71}',
    '\u{1B72}',
    '\u{1B73}',
    '\u{1CD0}',
    '\u{1CD1}',
    '\u{1CD2}',
    '\u{1CDA}',
    '\u{1CDB}',
    '\u{1CE0}',
    '\u{1DC0}',
    '\u{1DC1}',
    '\u{1DC3}',
    '\u{1DC4}',
    '\u{1DC5}',
    '\u{1DC6}',
    '\u{1DC7}',
    '\u{1DC8}',
    '\u{1DC9}',
    '\u{1DCB}',
    '\u{1DCC}',
    '\u{1DD1}',
    '\u{1DD2}',
    '\u{1DD3}',
    '\u{1DD4}',
    '\u{1DD5}',
    '\u{1DD6}',
    '\u{1DD7}',
    '\u{1DD8}',
    '\u{1DD9}',
    '\u{1DDA}',
    '\u{1DDB}',
    '\u{1DDC}',
    '\u{1DDD}',
    '\u{1DDE}',
    '\u{1DDF}',
    '\u{1DE0}',
    '\u{1DE1}',
    '\u{1DE2}',
    '\u{1DE3}',
    '\u{1DE4}',
    '\u{1DE5}',
    '\u{1DE6}',
    '\u{1DFE}',
    '\u{20D0}',
    '\u{20D1}',
    '\u{20D4}',
    '\u{20D5}',
    '\u{20D6}',
    '\u{20D7}',
    '\u{20DB}',
    '\u{20DC}',
    '\u{20E1}',
    '\u{20E7}',
    '\u{20E9}',
    '\u{20F0}',
    '\u{2CEF}',
    '\u{2CF0}',
    '\u{2CF1}',
    '\u{2DE0}',
    '\u{2DE1}',
    '\u{2DE2}',
    '\u{2DE3}',
    '\u{2DE4}',
    '\u{2DE5}',
    '\u{2DE6}',
    '\u{2DE7}',
    '\u{2DE8}',
    '\u{2DE9}',
    '\u{2DEA}',
    '\u{2DEB}',
    '\u{2DEC}',
    '\u{2DED}',
    '\u{2DEE}',
    '\u{2DEF}',
    '\u{2DF0}',
    '\u{2DF1}',
    '\u{2DF2}',
    '\u{2DF3}',
    '\u{2DF4}',
    '\u{2DF5}',
    '\u{2DF6}',
    '\u{2DF7}',
    '\u{2DF8}',
    '\u{2DF9}',
    '\u{2DFA}',
    '\u{2DFB}',
    '\u{2DFC}',
    '\u{2DFD}',
    '\u{2DFE}',
    '\u{2DFF}',
    '\u{A66F}',
    '\u{A67C}',
    '\u{A67D}',
    '\u{A6F0}',
    '\u{A6F1}',
    '\u{A8E0}',
    '\u{A8E1}',
    '\u{A8E2}',
    '\u{A8E3}',
    '\u{A8E4}',
    '\u{A8E5}',
    '\u{A8E6}',
    '\u{A8E7}',
    '\u{A8E8}',
    '\u{A8E9}',
    '\u{A8EA}',
    '\u{A8EB}',
    '\u{A8EC}',
    '\u{A8ED}',
    '\u{A8EE}',
    '\u{A8EF}',
    '\u{A8F0}',
    '\u{A8F1}',
    '\u{AAB0}',
    '\u{AAB2}',
    '\u{AAB3}',
    '\u{AAB7}',
    '\u{AAB8}',
    '\u{AABE}',
    '\u{AABF}',
    '\u{AAC1}',
    '\u{FE20}',
    '\u{FE21}',
    '\u{FE22}',
    '\u{FE23}',
    '\u{FE24}',
    '\u{FE25}',
    '\u{FE26}',
    '\u{10A0F}',
    '\u{10A38}',
    '\u{1D185}',
    '\u{1D186}',
    '\u{1D187}',
    '\u{1D188}',
    '\u{1D189}',
    '\u{1D1AA}',
    '\u{1D1AB}',
    '\u{1D1AC}',
    '\u{1D1AD}',
    '\u{1D242}',
    '\u{1D243}',
    '\u{1D244}',
];

#[inline]
fn diacritic(y: u16) -> char {
    *DIACRITICS
        .get(usize::from(y))
        .unwrap_or_else(|| &DIACRITICS[0])
}

/// Zero-copy POSIX shared memory (`t=s`) Kitty graphics protocol state.
pub struct KittyShmProtocol {
    segment: KittyShmSegment,
    transmit_str: String,
    size: Size,
    id_extra: u16,
    fg: Color,
}

impl KittyShmProtocol {
    /// Creates a new `KittyShmProtocol` from a canvas image and target dimensions.
    pub fn new(
        canvas: &DynamicImage,
        font_size: ratatui_image::FontSize,
        size: Size,
        is_tmux: bool,
    ) -> Result<Self, String> {
        if size.width == 0 || size.height == 0 {
            return Err("Target size has zero width or height".to_string());
        }
        let target_px_w = u32::from(size.width) * u32::from(font_size.width);
        let target_px_h = u32::from(size.height) * u32::from(font_size.height);

        let normalized;
        let rgba = if canvas.width() == target_px_w && canvas.height() == target_px_h {
            crate::imaging::to_rgba8_fast(canvas)
        } else {
            normalized = ratatui_image::Resize::Crop(None).resize(canvas, font_size, size, None);
            crate::imaging::to_rgba8_fast(&normalized)
        };

        let id = next_kitty_id();
        let [id_extra, id_r, id_g, id_b] = id.to_be_bytes();
        let fg = Color::Rgb(id_r, id_g, id_b);

        let segment = KittyShmSegment::new(rgba.as_raw())?;
        let b64_name = encode_base64_ascii(segment.shm_name().as_bytes());

        let (start, escape, end) =
            ratatui_image::picker::cap_parser::Parser::tmux_start_escape_end(is_tmux);
        let transmit_str = format!(
            "{start}{escape}_Gq=2,i={id},a=T,U=1,f=32,t=s,s={w},v={h},m=0;{b64_name}{escape}\\{end}",
            w = rgba.width(),
            h = rgba.height(),
        );

        Ok(Self {
            segment,
            transmit_str,
            size,
            id_extra: u16::from(id_extra),
            fg,
        })
    }

    /// Returns a reference to the transmission escape sequence.
    #[cfg(test)]
    pub fn transmit_str(&self) -> &str {
        &self.transmit_str
    }

    /// Renders the Kitty Unicode placeholder grid, prepending the transmission escape sequence
    /// only on the first render call.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        let max = DIACRITICS.len() as u16;
        let width = area.width.min(self.size.width).min(max);
        let height = area.height.min(self.size.height).min(max);

        let is_first_render = !self.segment.transmitted.swap(true, Ordering::SeqCst);
        if is_first_render && let Ok(mut ring) = TRANSMITTED_SHM_RING.lock() {
            ring.push_back(self.segment.c_name.clone());
            if ring.len() > 8
                && let Some(oldest) = ring.pop_front()
            {
                unsafe {
                    libc::shm_unlink(oldest.as_ptr());
                }
            }
        }

        let mut transmit = if is_first_render {
            Some(self.transmit_str.as_str())
        } else {
            None
        };

        let mut symbol = String::with_capacity(64);
        for y in 0..height {
            let row = y;
            for x in 0..width {
                let Some(cell) = buf.cell_mut((area.left() + x, area.top() + y)) else {
                    continue;
                };

                symbol.clear();
                if let Some(seq) = transmit.take() {
                    symbol.push_str(seq);
                }

                symbol.push(PLACEHOLDER);
                if x == 0 {
                    symbol.push(diacritic(row));
                    symbol.push(diacritic(x));
                    symbol.push(diacritic(self.id_extra));
                }

                cell.set_symbol(&symbol)
                    .set_fg(self.fg)
                    .set_diff_option(UNIT_WIDTH);
            }
        }
    }
}

/// Probes whether the terminal emulator supports Kitty graphics with POSIX shared memory (`t=s`).
///
/// Sends a test query to `stdout` referencing a temporary 4-byte shared memory object,
/// then polls `stdin` for a positive response (`_Gi=33;OK`).
/// Always unlinks the temporary shared memory object before returning.
pub fn probe_kitty_shm_support(is_tmux: bool) -> bool {
    let probe_name = format!("/imv-tui-probe-{}", std::process::id());
    let c_name = match CString::new(probe_name.clone()) {
        Ok(c) => c,
        Err(_) => return false,
    };

    let fd = unsafe {
        libc::shm_open(
            c_name.as_ptr(),
            libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
            0o600,
        )
    };
    if fd < 0 {
        return false;
    }

    let mut file = unsafe { File::from_raw_fd(fd) };
    let dummy = [0u8; 4];
    let _ = file.set_len(4);
    let _ = file.write_all(&dummy);
    drop(file);

    struct ShmUnlinkGuard(CString);
    impl Drop for ShmUnlinkGuard {
        fn drop(&mut self) {
            unsafe {
                libc::shm_unlink(self.0.as_ptr());
            }
        }
    }
    let _guard = ShmUnlinkGuard(c_name);

    let b64_name = encode_base64_ascii(probe_name.as_bytes());
    let (start, escape, end) =
        ratatui_image::picker::cap_parser::Parser::tmux_start_escape_end(is_tmux);
    let query =
        format!("{start}{escape}_Gi=33,s=1,v=1,a=q,t=s,f=32;{b64_name}{escape}\\{escape}[5n{end}");

    let mut stdout = std::io::stdout();
    if stdout.write_all(query.as_bytes()).is_err() || stdout.flush().is_err() {
        return false;
    }

    let mut poll_fd = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };

    let start_time = std::time::Instant::now();
    let timeout = std::time::Duration::from_millis(150);
    let mut response = Vec::with_capacity(256);
    let mut chunk = [0u8; 128];

    loop {
        let elapsed = start_time.elapsed();
        if elapsed >= timeout {
            break;
        }
        let remaining = (timeout - elapsed).as_millis() as libc::c_int;
        let ret = unsafe { libc::poll(&mut poll_fd, 1, remaining) };
        if ret <= 0 {
            break;
        }
        if (poll_fd.revents & libc::POLLIN) != 0 {
            let n = unsafe {
                libc::read(
                    libc::STDIN_FILENO,
                    chunk.as_mut_ptr() as *mut libc::c_void,
                    chunk.len(),
                )
            };
            if n <= 0 {
                break;
            }
            response.extend_from_slice(&chunk[..n as usize]);
            if response.windows(4).any(|w| w == b"\x1b[0n") {
                break;
            }
        }
    }

    response.windows(9).any(|w| w == b"_Gi=33;OK")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ActiveProtocol, KittyTransferMode};
    use image::RgbaImage;
    use ratatui::buffer::Buffer;
    use ratatui::layout::{Rect, Size};
    use ratatui_image::FontSize;

    #[test]
    fn test_base64_ascii_rfc4648_test_vectors() {
        assert_eq!(encode_base64_ascii(b""), "");
        assert_eq!(encode_base64_ascii(b"f"), "Zg==");
        assert_eq!(encode_base64_ascii(b"fo"), "Zm8=");
        assert_eq!(encode_base64_ascii(b"foo"), "Zm9v");
        assert_eq!(encode_base64_ascii(b"foob"), "Zm9vYg==");
        assert_eq!(encode_base64_ascii(b"fooba"), "Zm9vYmE=");
        assert_eq!(encode_base64_ascii(b"foobar"), "Zm9vYmFy");

        // Verify with typical shared memory path
        let shm_path = b"/imv-tui-12345-67";
        let encoded = encode_base64_ascii(shm_path);
        assert_eq!(encoded, "L2ltdi10dWktMTIzNDUtNjc=");
    }

    #[test]
    fn test_kitty_shm_protocol_lifecycle_transmission_and_drop_cleanup() {
        let raw = vec![255u8; 10 * 10 * 4];
        let img = DynamicImage::ImageRgba8(RgbaImage::from_raw(10, 10, raw.clone()).unwrap());
        let font_size = FontSize {
            width: 1,
            height: 1,
        };
        let size = Size::new(10, 10);

        // 1. Create protocol: must create a valid POSIX shm segment with t=s transmission string
        let proto =
            KittyShmProtocol::new(&img, font_size, size, false).expect("SHM creation failed");
        assert!(proto.transmit_str().contains("t=s"));
        assert!(proto.transmit_str().contains("f=32"));
        assert!(proto.transmit_str().contains("a=T,U=1"));
        assert!(!proto.segment.is_transmitted());

        // 2. Render to buffer: first render must inject transmission escape sequence into (0, 0)
        let area = Rect::new(0, 0, 10, 10);
        let mut buf = Buffer::empty(area);
        proto.render(area, &mut buf);
        assert!(proto.segment.is_transmitted());

        let first_cell = &buf[(0, 0)];
        assert!(
            first_cell.symbol().contains("_Gq=2"),
            "Expected transmission string in cell (0, 0)"
        );
        assert!(
            first_cell.symbol().contains(PLACEHOLDER),
            "Expected placeholder character in cell (0, 0)"
        );

        // Subsequent cell in row 0 inherits position without extra diacritics
        let second_cell = &buf[(1, 0)];
        assert_eq!(second_cell.symbol(), PLACEHOLDER.to_string());

        // 3. Second render: one-shot transmission must NOT re-emit transmission escape sequence
        let mut buf2 = Buffer::empty(area);
        proto.render(area, &mut buf2);
        let first_cell2 = &buf2[(0, 0)];
        assert!(
            !first_cell2.symbol().contains("_Gq=2"),
            "Second render should not re-transmit escape sequence"
        );
        assert!(first_cell2.symbol().starts_with(PLACEHOLDER));

        // 4. Verify unrendered segment is unlinked on Drop
        let unrendered_seg = KittyShmSegment::new(b"test unrendered bytes").unwrap();
        let unrendered_c_name = unrendered_seg.c_name.clone();
        let fd_open = unsafe { libc::shm_open(unrendered_c_name.as_ptr(), libc::O_RDONLY, 0) };
        assert!(fd_open >= 0, "Shared memory segment must exist before drop");
        unsafe {
            libc::close(fd_open);
        }

        drop(unrendered_seg);

        let fd_after_drop =
            unsafe { libc::shm_open(unrendered_c_name.as_ptr(), libc::O_RDONLY, 0) };
        assert!(
            fd_after_drop < 0,
            "Unrendered shared memory segment must be unlinked on drop"
        );

        // 5. Cleanup transmitted segments
        cleanup_all_kitty_shm_segments();
    }

    #[test]
    fn test_active_protocol_display_label_and_shm_helpers() {
        let proto = ActiveProtocol::Kitty(KittyTransferMode::SharedMemory);
        assert_eq!(proto.display_label(), "Kitty (SHM)");
        assert!(proto.uses_kitty_shm());

        let proto_off = proto.with_kitty_shm(false);
        assert_eq!(proto_off.display_label(), "Kitty");
        assert!(!proto_off.uses_kitty_shm());

        let proto_comp = ActiveProtocol::Kitty(KittyTransferMode::Compressed);
        assert_eq!(proto_comp.display_label(), "Kitty (Compressed)");
        assert!(!proto_comp.uses_kitty_shm());

        // with_kitty_shm does not enable shm when compression is true
        let proto_comp_attempt = proto_comp.with_kitty_shm(true);
        assert_eq!(proto_comp_attempt.display_label(), "Kitty (Compressed)");
        assert!(!proto_comp_attempt.uses_kitty_shm());
    }
}
