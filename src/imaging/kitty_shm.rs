//! Zero-copy POSIX shared memory (`t=s`) Kitty graphics protocol implementation.
//!
//! Provides high-throughput direct frame transmission via `/dev/shm` shared memory objects,
//! eliminating base64 encoding and terminal pipe bandwidth bottlenecks.

use std::collections::VecDeque;
use std::ffi::CString;
use std::fs::File;
use std::io::Write;
use std::num::NonZeroU16;
use std::os::fd::{AsRawFd, FromRawFd};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use image::DynamicImage;
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::{Rect, Size};
use ratatui::style::Color;
use rayon::prelude::*;

/// Standard RFC 4648 Base64 encoding for ASCII byte sequences.
pub fn encode_base64_ascii(bytes: &[u8]) -> String {
    base64_simd::STANDARD.encode_to_string(bytes)
}

static TRANSMITTED_SHM_RING: Mutex<VecDeque<CString>> = Mutex::new(VecDeque::new());
static SHM_SEQ: AtomicU64 = AtomicU64::new(1);
static NEXT_KITTY_ID: AtomicU32 = AtomicU32::new(100);

/// Returns the next unique 24-bit+extra Kitty graphics image identifier.
pub fn next_kitty_id() -> u32 {
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

/// Minimum byte length before `KittyShmSegment::new` copies into the `mmap`'d SHM region
/// in parallel across `rayon` worker threads.
const PARALLEL_COPY_MIN_BYTES: usize = 256 * 1024;

/// Minimum pixel count before `KittyShmSegment::new_packed_rgb` packs RGBA32 -> RGB24 directly
/// into the `mmap`'d SHM region in parallel across `rayon` worker threads.
const PARALLEL_PACK_MIN_PIXELS: usize = 65_536;

/// RAII guard for a POSIX `mmap` mapping that unmaps the region on drop.
struct MmapGuard {
    ptr: *mut libc::c_void,
    len: usize,
}

impl MmapGuard {
    /// Returns a mutable byte slice covering the mapped region, binding the slice lifetime
    /// to `&mut self` so `munmap` cannot run while the slice is live.
    #[inline]
    fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: `MmapGuard` is only constructed from a valid, non-null, non-`MAP_FAILED`
        // `mmap` region of `self.len` writable bytes on an exclusively opened SHM object,
        // and the returned slice borrows `&mut self`.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.cast::<u8>(), self.len) }
    }
}

impl Drop for MmapGuard {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.ptr, self.len);
        }
    }
}

/// Packs a slice of 4-pixel RGBA blocks (`[u8; 16]`) into 4-pixel RGB blocks (`[u8; 12]`).
#[inline]
fn pack_rgba_blocks(in_blocks: &[[u8; 16]], out_blocks: &mut [[u8; 12]]) {
    for (src, dst) in in_blocks.iter().zip(out_blocks.iter_mut()) {
        *dst = [
            src[0], src[1], src[2], src[4], src[5], src[6], src[8], src[9], src[10], src[12],
            src[13], src[14],
        ];
    }
}

/// A POSIX shared memory segment holding raw RGB or RGBA pixel data for Kitty transmission.
pub struct KittyShmSegment {
    c_name: CString,
    len: usize,
    transmitted: AtomicBool,
}

impl KittyShmSegment {
    /// Creates a new POSIX shared memory object of `len` bytes and populates it via `fill`.
    ///
    /// Maps the shared memory object directly into the process address space with `mmap` so
    /// callers can write (or parallelize writes via `rayon`) without an intermediate kernel
    /// `write(2)` buffer copy, gracefully falling back to `write_all` if `mmap` fails.
    pub fn create_and_fill(len: usize, fill: impl FnOnce(&mut [u8])) -> Result<Self, String> {
        let seq = SHM_SEQ.fetch_add(1, Ordering::Relaxed);
        let shm_name = format!("/imv-tui-{}-{}", std::process::id(), seq);
        let c_name =
            CString::new(shm_name).map_err(|e| format!("Invalid CString for shm_name: {}", e))?;

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
                c_name.to_str().unwrap_or(""),
                std::io::Error::last_os_error()
            ));
        }

        // Construct `segment` immediately after `shm_open` succeeds so its `Drop` impl
        // automatically unlinks the SHM object on any early error or panic inside `fill`
        // (in reverse drop order: `MmapGuard` munmap -> `File` close -> `segment` shm_unlink).
        let segment = Self {
            c_name,
            len,
            transmitted: AtomicBool::new(false),
        };

        let mut file = unsafe { File::from_raw_fd(fd) };
        if let Err(e) = file.set_len(len as u64) {
            return Err(format!(
                "Failed to set shm size for {}: {}",
                segment.shm_name(),
                e
            ));
        }

        if len > 0 {
            let ptr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    len,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    file.as_raw_fd(),
                    0,
                )
            };
            if ptr == libc::MAP_FAILED || ptr.is_null() {
                let mut buf = vec![0u8; len];
                fill(&mut buf);
                if let Err(e) = file.write_all(&buf) {
                    return Err(format!(
                        "Failed to write bytes to shm {}: {}",
                        segment.shm_name(),
                        e
                    ));
                }
            } else {
                let mut guard = MmapGuard { ptr, len };
                fill(guard.as_mut_slice());
            }
        } else {
            fill(&mut []);
        }
        drop(file);

        Ok(segment)
    }

    /// Creates a new POSIX shared memory object and writes the raw pixel bytes into it.
    ///
    /// Uses `rayon` parallel page-aligned `mmap` writes for payloads `>= 256 KiB` and a
    /// direct `copy_from_slice` below that threshold.
    pub fn new(raw_bytes: &[u8]) -> Result<Self, String> {
        Self::create_and_fill(raw_bytes.len(), |mapped| {
            if raw_bytes.len() >= PARALLEL_COPY_MIN_BYTES {
                let target_slabs = rayon::current_num_threads().max(1) * 4;
                // Align slab size to 4 KiB page boundaries so parallel workers fault
                // disjoint tmpfs pages without contention.
                let slab_size = raw_bytes.len().div_ceil(target_slabs).div_ceil(4096) * 4096;
                mapped
                    .par_chunks_mut(slab_size)
                    .enumerate()
                    .for_each(|(i, dst)| {
                        let start = i * slab_size;
                        dst.copy_from_slice(&raw_bytes[start..start + dst.len()]);
                    });
            } else {
                mapped.copy_from_slice(raw_bytes);
            }
        })
    }

    /// Creates a new POSIX shared memory object by packing 32-bit RGBA (`[R, G, B, A]`) bytes
    /// directly into packed 24-bit RGB (`[R, G, B]`) bytes inside the `mmap`'d SHM segment.
    ///
    /// Avoids allocating an intermediate RGB buffer and parallelizes 4-pixel (16 -> 12 byte)
    /// block packing across `rayon` workers when `num_pixels >= 65_536`.
    pub fn new_packed_rgb(rgba_bytes: &[u8]) -> Result<Self, String> {
        let num_pixels = rgba_bytes.len() / 4;
        let rgb_len = num_pixels * 3;
        Self::create_and_fill(rgb_len, |mapped| {
            let (in_chunks, in_rem) = rgba_bytes[..num_pixels * 4].as_chunks::<16>();
            let (out_chunks, out_rem) = mapped.as_chunks_mut::<12>();

            if num_pixels >= PARALLEL_PACK_MIN_PIXELS {
                let target_slabs = rayon::current_num_threads().max(1) * 4;
                let slab_blocks = out_chunks.len().div_ceil(target_slabs).max(1);
                out_chunks.par_chunks_mut(slab_blocks).enumerate().for_each(
                    |(slab_idx, dst_slab)| {
                        let start = slab_idx * slab_blocks;
                        let src_slab = &in_chunks[start..start + dst_slab.len()];
                        pack_rgba_blocks(src_slab, dst_slab);
                    },
                );
            } else {
                pack_rgba_blocks(in_chunks, out_chunks);
            }

            for (src, dst) in in_rem
                .as_chunks::<4>()
                .0
                .iter()
                .zip(out_rem.as_chunks_mut::<3>().0.iter_mut())
            {
                *dst = [src[0], src[1], src[2]];
            }
        })
    }

    /// Returns the shared memory object name.
    pub fn shm_name(&self) -> &str {
        self.c_name.to_str().unwrap_or("")
    }

    /// Returns `true` if the frame has been transmitted to the terminal.
    #[cfg(test)]
    pub fn is_transmitted(&self) -> bool {
        self.transmitted.load(Ordering::Acquire)
    }

    /// Reads the raw bytes currently stored in this POSIX shared memory segment.
    ///
    /// Uses `mmap` rather than `read(2)` because macOS/BSD POSIX shared memory descriptors
    /// reject `read(2)` with `ENXIO` and page-align `fstat().st_size`.
    #[cfg(test)]
    pub fn read_bytes(&self) -> Vec<u8> {
        let fd = unsafe { libc::shm_open(self.c_name.as_ptr(), libc::O_RDONLY, 0) };
        assert!(fd >= 0, "Failed to open shm segment {}", self.shm_name());
        let _file = unsafe { File::from_raw_fd(fd) };
        if self.len == 0 {
            return Vec::new();
        }
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                self.len,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        assert!(
            ptr != libc::MAP_FAILED && !ptr.is_null(),
            "Failed to mmap shm segment {}: {}",
            self.shm_name(),
            std::io::Error::last_os_error()
        );
        let guard = MmapGuard { ptr, len: self.len };
        // SAFETY: `guard` owns a valid read-only mapping of `self.len` bytes backed by `_file`
        // and unmaps on drop after `to_vec()` copies out the slice.
        let out = unsafe { std::slice::from_raw_parts(guard.ptr.cast::<u8>(), guard.len) }.to_vec();
        drop(guard);
        out
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

/// Renders a Kitty Unicode placeholder grid (`U+10EEEE`) into `buf` within `area`,
/// optionally prepending a one-shot graphics transmission escape sequence to the top-left cell.
pub fn render_kitty_placeholders(
    area: Rect,
    buf: &mut Buffer,
    size: Size,
    fg: Color,
    id_extra: u16,
    mut transmit: Option<&str>,
) {
    let max = DIACRITICS.len() as u16;
    let width = area.width.min(size.width).min(max);
    let height = area.height.min(size.height).min(max);

    const PLACEHOLDER_STR: &str = "\u{10EEEE}";
    let mut symbol = String::with_capacity(64);
    for y in 0..height {
        let row = y;
        for x in 0..width {
            let Some(cell) = buf.cell_mut((area.left() + x, area.top() + y)) else {
                continue;
            };

            if let Some(seq) = transmit.take() {
                let mut first_symbol = String::with_capacity(seq.len() + 32);
                first_symbol.push_str(seq);
                first_symbol.push(PLACEHOLDER);
                if x == 0 {
                    first_symbol.push(diacritic(row));
                    first_symbol.push(diacritic(x));
                    first_symbol.push(diacritic(id_extra));
                }
                cell.set_symbol(&first_symbol)
                    .set_fg(fg)
                    .set_diff_option(UNIT_WIDTH);
                continue;
            }

            if x > 0 {
                cell.set_symbol(PLACEHOLDER_STR)
                    .set_fg(fg)
                    .set_diff_option(UNIT_WIDTH);
                continue;
            }

            symbol.clear();
            symbol.push(PLACEHOLDER);
            symbol.push(diacritic(row));
            symbol.push(diacritic(x));
            symbol.push(diacritic(id_extra));

            cell.set_symbol(&symbol)
                .set_fg(fg)
                .set_diff_option(UNIT_WIDTH);
        }
    }
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
    /// Assembles a [`KittyShmProtocol`] from an already-populated [`KittyShmSegment`].
    fn from_segment(
        segment: KittyShmSegment,
        width: u32,
        height: u32,
        pixel_fmt: u8,
        size: Size,
        is_tmux: bool,
    ) -> Self {
        let id = next_kitty_id();
        let [id_extra, id_r, id_g, id_b] = id.to_be_bytes();
        let fg = Color::Rgb(id_r, id_g, id_b);

        let b64_name = encode_base64_ascii(segment.shm_name().as_bytes());

        let byte_len = segment.len;
        let (start, escape, end) = super::picker::tmux_start_escape_end(is_tmux);
        let transmit_str = format!(
            "{start}{escape}_Gq=2,i={id},a=T,U=1,f={pixel_fmt},t=s,s={width},v={height},S={byte_len},m=0;{b64_name}{escape}\\{end}",
        );

        Self {
            segment,
            transmit_str,
            size,
            id_extra: u16::from(id_extra),
            fg,
        }
    }

    /// Creates a new `KittyShmProtocol` from a canvas image and target dimensions.
    ///
    /// Automatically selects `f=24` (RGB8) for opaque images (including zero-intermediate-allocation
    /// RGBA32 -> RGB24 packing directly into the `mmap`'d `/dev/shm` segment) and `f=32` (RGBA8)
    /// for images containing transparency.
    pub fn new(
        canvas: &DynamicImage,
        font_size: super::picker::FontSize,
        size: Size,
        is_tmux: bool,
    ) -> Result<Self, String> {
        if size.width == 0 || size.height == 0 {
            return Err("Target size has zero width or height".to_string());
        }

        let source = super::protocol::normalize_canvas_to_cells(canvas, font_size, size);
        let source = source.as_ref();

        let width = source.width();
        let height = source.height();
        if width == 0 || height == 0 {
            return Err("Source image has zero width or height".to_string());
        }

        let (segment, pixel_fmt) = match source {
            DynamicImage::ImageRgb8(rgb) => (KittyShmSegment::new(rgb.as_raw())?, 24u8),
            DynamicImage::ImageRgba8(rgba) => {
                if super::types::is_rgba8_opaque(rgba.as_raw()) {
                    (KittyShmSegment::new_packed_rgb(rgba.as_raw())?, 24u8)
                } else {
                    (KittyShmSegment::new(rgba.as_raw())?, 32u8)
                }
            }
            other => {
                let rgba = super::types::to_rgba8_fast(other);
                if super::types::is_rgba8_opaque(rgba.as_raw()) {
                    (KittyShmSegment::new_packed_rgb(rgba.as_raw())?, 24u8)
                } else {
                    (KittyShmSegment::new(rgba.as_raw())?, 32u8)
                }
            }
        };

        Ok(Self::from_segment(
            segment, width, height, pixel_fmt, size, is_tmux,
        ))
    }

    /// Creates a new `f=32` (RGBA8) `KittyShmProtocol` by letting `fill` write directly into the
    /// zero-initialized `mmap`'d `/dev/shm` pixel buffer.
    ///
    /// Eliminates intermediate full-viewport RGBA canvas allocations, zero-fills, and secondary
    /// copies for letterboxed or panned frames: untouched margin bytes remain transparent zeros
    /// (`[0, 0, 0, 0]`) as guaranteed by `shm_open` + `ftruncate`.
    pub fn from_rgba_writer(
        font_size: super::picker::FontSize,
        size: Size,
        is_tmux: bool,
        fill: impl FnOnce(&mut [u8], u32, u32),
    ) -> Result<Self, String> {
        if size.width == 0 || size.height == 0 {
            return Err("Target size has zero width or height".to_string());
        }

        let target_px_w = u32::from(size.width) * u32::from(font_size.width);
        let target_px_h = u32::from(size.height) * u32::from(font_size.height);
        if target_px_w == 0 || target_px_h == 0 {
            return Err("Target pixel dimensions have zero width or height".to_string());
        }

        let byte_len = (target_px_w as usize)
            .checked_mul(target_px_h as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| "Target pixel buffer size overflow".to_string())?;

        let segment = KittyShmSegment::create_and_fill(byte_len, |mapped| {
            fill(mapped, target_px_w, target_px_h);
        })?;

        Ok(Self::from_segment(
            segment,
            target_px_w,
            target_px_h,
            32,
            size,
            is_tmux,
        ))
    }

    /// Returns a reference to the transmission escape sequence.
    #[cfg(test)]
    pub fn transmit_str(&self) -> &str {
        &self.transmit_str
    }

    /// Reads the raw pixel bytes stored in this protocol's POSIX shared memory segment.
    #[cfg(test)]
    pub fn read_shm_bytes(&self) -> Vec<u8> {
        self.segment.read_bytes()
    }

    /// Renders the Kitty Unicode placeholder grid, prepending the transmission escape sequence
    /// only on the first render call.
    pub fn render(&self, area: Rect, buf: &mut Buffer) {
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

        let transmit = if is_first_render {
            Some(self.transmit_str.as_str())
        } else {
            None
        };

        render_kitty_placeholders(area, buf, self.size, self.fg, self.id_extra, transmit);
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

    struct ShmUnlinkGuard(CString);
    impl Drop for ShmUnlinkGuard {
        fn drop(&mut self) {
            unsafe {
                libc::shm_unlink(self.0.as_ptr());
            }
        }
    }
    let _guard = ShmUnlinkGuard(c_name);

    let file = unsafe { File::from_raw_fd(fd) };
    // POSIX `ftruncate` (`set_len`) zero-initializes newly extended SHM pages, avoiding
    // `write(2)` which returns `ENXIO` on macOS/BSD POSIX shared memory descriptors.
    if file.set_len(4).is_err() {
        return false;
    }
    drop(file);

    let b64_name = encode_base64_ascii(probe_name.as_bytes());
    let (start, escape, end) = super::picker::tmux_start_escape_end(is_tmux);
    let query = format!(
        "{start}{escape}_Gi=33,s=1,v=1,a=q,t=s,f=32,S=4;{b64_name}{escape}\\{escape}[5n{end}"
    );

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
    use super::super::picker::FontSize;
    use super::*;
    use image::{GrayAlphaImage, GrayImage, RgbImage, RgbaImage};
    use ratatui::buffer::Buffer;
    use ratatui::layout::{Rect, Size};

    fn read_shm_bytes(segment: &KittyShmSegment) -> Vec<u8> {
        segment.read_bytes()
    }

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
        let font_size = FontSize {
            width: 1,
            height: 1,
        };
        let size = Size::new(10, 10);
        let area = Rect::new(0, 0, 10, 10);

        // 1. Opaque ImageRgba8 must automatically select f=24 with t=s transmission string
        let opaque_raw = vec![255u8; 10 * 10 * 4];
        let opaque_img = DynamicImage::ImageRgba8(RgbaImage::from_raw(10, 10, opaque_raw).unwrap());
        let proto_opaque = KittyShmProtocol::new(&opaque_img, font_size, size, false)
            .expect("Opaque SHM creation failed");
        assert!(proto_opaque.transmit_str().contains("t=s"));
        assert!(proto_opaque.transmit_str().contains("f=24"));
        assert!(proto_opaque.transmit_str().contains("S=300"));
        assert!(proto_opaque.transmit_str().contains("a=T,U=1"));
        assert!(!proto_opaque.segment.is_transmitted());

        // First render must inject transmission escape sequence into (0, 0)
        let mut buf = Buffer::empty(area);
        proto_opaque.render(area, &mut buf);
        assert!(proto_opaque.segment.is_transmitted());

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

        // Second render: one-shot transmission must NOT re-emit transmission escape sequence
        let mut buf2 = Buffer::empty(area);
        proto_opaque.render(area, &mut buf2);
        let first_cell2 = &buf2[(0, 0)];
        assert!(
            !first_cell2.symbol().contains("_Gq=2"),
            "Second render should not re-transmit escape sequence"
        );
        assert!(first_cell2.symbol().starts_with(PLACEHOLDER));

        // 2. Transparent ImageRgba8 must automatically select f=32
        let mut transparent_raw = vec![255u8; 10 * 10 * 4];
        transparent_raw[3] = 128;
        let transparent_img =
            DynamicImage::ImageRgba8(RgbaImage::from_raw(10, 10, transparent_raw).unwrap());
        let proto_transparent = KittyShmProtocol::new(&transparent_img, font_size, size, false)
            .expect("Transparent SHM creation failed");
        assert!(proto_transparent.transmit_str().contains("t=s"));
        assert!(proto_transparent.transmit_str().contains("f=32"));
        assert!(proto_transparent.transmit_str().contains("a=T,U=1"));
        assert!(!proto_transparent.segment.is_transmitted());

        let mut buf3 = Buffer::empty(area);
        proto_transparent.render(area, &mut buf3);
        assert!(proto_transparent.segment.is_transmitted());
        assert!(buf3[(0, 0)].symbol().contains("f=32"));

        let mut buf4 = Buffer::empty(area);
        proto_transparent.render(area, &mut buf4);
        assert!(!buf4[(0, 0)].symbol().contains("_Gq=2"));

        // 3. Verify unrendered segment is unlinked on Drop
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

        // 4. Cleanup transmitted segments
        cleanup_all_kitty_shm_segments();
    }

    #[test]
    fn test_kitty_shm_rgb8_small_and_parallel_readback() {
        // Small (< 256 KiB sequential mmap copy path)
        let small_w = 7u32;
        let small_h = 5u32;
        let small_raw: Vec<u8> = (0..(small_w * small_h * 3) as usize)
            .map(|i| ((i * 37 + 13) & 0xFF) as u8)
            .collect();
        let small_img = DynamicImage::ImageRgb8(
            RgbImage::from_raw(small_w, small_h, small_raw.clone()).unwrap(),
        );
        let proto_small = KittyShmProtocol::new(
            &small_img,
            FontSize {
                width: 1,
                height: 1,
            },
            Size::new(small_w as u16, small_h as u16),
            false,
        )
        .expect("Small RGB8 SHM creation failed");
        assert!(proto_small.transmit_str().contains("f=24"));
        assert!(proto_small.transmit_str().contains("s=7,v=5"));
        assert_eq!(read_shm_bytes(&proto_small.segment), small_raw);
        drop(proto_small);

        // Large (>= 256 KiB parallel rayon mmap copy path: 400 * 300 * 3 = 360,000 bytes)
        let large_w = 400u32;
        let large_h = 300u32;
        let large_raw: Vec<u8> = (0..(large_w * large_h * 3) as usize)
            .map(|i| ((i * 53 + 7) & 0xFF) as u8)
            .collect();
        assert!(large_raw.len() >= PARALLEL_COPY_MIN_BYTES);
        let large_img = DynamicImage::ImageRgb8(
            RgbImage::from_raw(large_w, large_h, large_raw.clone()).unwrap(),
        );
        let proto_large = KittyShmProtocol::new(
            &large_img,
            FontSize {
                width: 4,
                height: 3,
            },
            Size::new(100, 100),
            false,
        )
        .expect("Large RGB8 SHM creation failed");
        assert!(proto_large.transmit_str().contains("f=24"));
        assert!(proto_large.transmit_str().contains("s=400,v=300"));
        assert_eq!(read_shm_bytes(&proto_large.segment), large_raw);
        drop(proto_large);
    }

    #[test]
    fn test_kitty_shm_opaque_rgba8_small_and_parallel_packed_rgb_readback() {
        // Tail remainder coverage for 0, 1, 2, 3 pixels via new_packed_rgb
        for num_pixels in 0..=7usize {
            let mut rgba = Vec::with_capacity(num_pixels * 4);
            let mut expected_rgb = Vec::with_capacity(num_pixels * 3);
            for p in 0..num_pixels {
                let r = ((p * 29 + 1) & 0xFF) as u8;
                let g = ((p * 43 + 2) & 0xFF) as u8;
                let b = ((p * 61 + 3) & 0xFF) as u8;
                rgba.extend_from_slice(&[r, g, b, 255]);
                expected_rgb.extend_from_slice(&[r, g, b]);
            }
            let seg = KittyShmSegment::new_packed_rgb(&rgba).expect("Packed RGB SHM failed");
            assert_eq!(read_shm_bytes(&seg), expected_rgb);
            drop(seg);
        }

        // Small opaque ImageRgba8 with non-multiple-of-4 pixel count (7 * 5 = 35 pixels, 35 % 4 == 3)
        let small_w = 7u32;
        let small_h = 5u32;
        let small_pixels = (small_w * small_h) as usize;
        let mut small_rgba = Vec::with_capacity(small_pixels * 4);
        let mut small_expected_rgb = Vec::with_capacity(small_pixels * 3);
        for p in 0..small_pixels {
            let r = ((p * 17 + 5) & 0xFF) as u8;
            let g = ((p * 31 + 11) & 0xFF) as u8;
            let b = ((p * 47 + 23) & 0xFF) as u8;
            small_rgba.extend_from_slice(&[r, g, b, 255]);
            small_expected_rgb.extend_from_slice(&[r, g, b]);
        }
        let small_img =
            DynamicImage::ImageRgba8(RgbaImage::from_raw(small_w, small_h, small_rgba).unwrap());
        let proto_small = KittyShmProtocol::new(
            &small_img,
            FontSize {
                width: 1,
                height: 1,
            },
            Size::new(small_w as u16, small_h as u16),
            false,
        )
        .expect("Small opaque RGBA8 SHM creation failed");
        assert!(proto_small.transmit_str().contains("f=24"));
        assert!(proto_small.transmit_str().contains("s=7,v=5"));
        assert_eq!(read_shm_bytes(&proto_small.segment), small_expected_rgb);
        drop(proto_small);

        // Large opaque ImageRgba8 (>= 65_536 pixels parallel rayon mmap pack path,
        // 301 * 251 = 75_551 pixels, 75_551 % 4 == 3 to also exercise tail remainder)
        let large_w = 301u32;
        let large_h = 251u32;
        let large_pixels = (large_w * large_h) as usize;
        assert!(large_pixels >= PARALLEL_PACK_MIN_PIXELS);
        assert_eq!(large_pixels % 4, 3);
        let mut large_rgba = Vec::with_capacity(large_pixels * 4);
        let mut large_expected_rgb = Vec::with_capacity(large_pixels * 3);
        for p in 0..large_pixels {
            let r = ((p * 13 + 3) & 0xFF) as u8;
            let g = ((p * 29 + 7) & 0xFF) as u8;
            let b = ((p * 59 + 19) & 0xFF) as u8;
            large_rgba.extend_from_slice(&[r, g, b, 255]);
            large_expected_rgb.extend_from_slice(&[r, g, b]);
        }
        let large_img =
            DynamicImage::ImageRgba8(RgbaImage::from_raw(large_w, large_h, large_rgba).unwrap());
        let proto_large = KittyShmProtocol::new(
            &large_img,
            FontSize {
                width: 1,
                height: 1,
            },
            Size::new(large_w as u16, large_h as u16),
            false,
        )
        .expect("Large opaque RGBA8 SHM creation failed");
        assert!(proto_large.transmit_str().contains("f=24"));
        assert!(proto_large.transmit_str().contains("s=301,v=251"));
        assert_eq!(read_shm_bytes(&proto_large.segment), large_expected_rgb);
        drop(proto_large);
    }

    #[test]
    fn test_kitty_shm_transparent_rgba8_small_and_parallel_readback() {
        // Small transparent ImageRgba8 (< 256 KiB sequential mmap copy path)
        let small_w = 7u32;
        let small_h = 5u32;
        let mut small_raw: Vec<u8> = (0..(small_w * small_h * 4) as usize)
            .map(|i| {
                if i % 4 == 3 {
                    255
                } else {
                    ((i * 19 + 5) & 0xFF) as u8
                }
            })
            .collect();
        *small_raw.last_mut().unwrap() = 254;
        let small_img = DynamicImage::ImageRgba8(
            RgbaImage::from_raw(small_w, small_h, small_raw.clone()).unwrap(),
        );
        let proto_small = KittyShmProtocol::new(
            &small_img,
            FontSize {
                width: 1,
                height: 1,
            },
            Size::new(small_w as u16, small_h as u16),
            false,
        )
        .expect("Small transparent RGBA8 SHM creation failed");
        assert!(proto_small.transmit_str().contains("f=32"));
        assert!(proto_small.transmit_str().contains("s=7,v=5"));
        assert_eq!(read_shm_bytes(&proto_small.segment), small_raw);
        drop(proto_small);

        // Large transparent ImageRgba8 (>= 256 KiB parallel rayon mmap copy path:
        // 300 * 300 * 4 = 360,000 bytes)
        let large_w = 300u32;
        let large_h = 300u32;
        let mut large_raw: Vec<u8> = (0..(large_w * large_h * 4) as usize)
            .map(|i| {
                if i % 4 == 3 {
                    255
                } else {
                    ((i * 41 + 9) & 0xFF) as u8
                }
            })
            .collect();
        assert!(large_raw.len() >= PARALLEL_COPY_MIN_BYTES);
        let mid_alpha_idx = (large_raw.len() / 2) | 3;
        large_raw[mid_alpha_idx] = 0;
        let large_img = DynamicImage::ImageRgba8(
            RgbaImage::from_raw(large_w, large_h, large_raw.clone()).unwrap(),
        );
        let proto_large = KittyShmProtocol::new(
            &large_img,
            FontSize {
                width: 3,
                height: 3,
            },
            Size::new(100, 100),
            false,
        )
        .expect("Large transparent RGBA8 SHM creation failed");
        assert!(proto_large.transmit_str().contains("f=32"));
        assert!(proto_large.transmit_str().contains("s=300,v=300"));
        assert_eq!(read_shm_bytes(&proto_large.segment), large_raw);
        drop(proto_large);
    }

    #[test]
    fn test_kitty_shm_other_dynamic_image_variants() {
        let font_size = FontSize {
            width: 1,
            height: 1,
        };
        let size = Size::new(5, 3);

        // Opaque Luma8 -> f=24 packed RGB
        let luma_raw: Vec<u8> = (0..15u8).map(|v| v * 17).collect();
        let luma_img =
            DynamicImage::ImageLuma8(GrayImage::from_raw(5, 3, luma_raw.clone()).unwrap());
        let proto_luma = KittyShmProtocol::new(&luma_img, font_size, size, false)
            .expect("Luma8 SHM creation failed");
        assert!(proto_luma.transmit_str().contains("f=24"));
        let expected_rgb: Vec<u8> = luma_raw.iter().flat_map(|&l| [l, l, l]).collect();
        assert_eq!(read_shm_bytes(&proto_luma.segment), expected_rgb);
        drop(proto_luma);

        // Transparent LumaA8 -> f=32 RGBA
        let mut lumaa_raw = Vec::with_capacity(30);
        let mut expected_rgba = Vec::with_capacity(60);
        for i in 0..15u8 {
            let l = i * 16;
            let a = if i == 7 { 100 } else { 255 };
            lumaa_raw.extend_from_slice(&[l, a]);
            expected_rgba.extend_from_slice(&[l, l, l, a]);
        }
        let lumaa_img =
            DynamicImage::ImageLumaA8(GrayAlphaImage::from_raw(5, 3, lumaa_raw).unwrap());
        let proto_lumaa = KittyShmProtocol::new(&lumaa_img, font_size, size, false)
            .expect("LumaA8 SHM creation failed");
        assert!(proto_lumaa.transmit_str().contains("f=32"));
        assert_eq!(read_shm_bytes(&proto_lumaa.segment), expected_rgba);
        drop(proto_lumaa);
    }

    #[test]
    fn test_kitty_shm_from_rgba_writer_zero_margins_and_error_bounds() {
        let font_size = FontSize::new(4, 3);
        let size = Size::new(5, 4); // 20 x 12 pixels

        // Zero cell or font dimensions must fail cleanly.
        assert!(
            KittyShmProtocol::from_rgba_writer(font_size, Size::new(0, 4), false, |_, _, _| {})
                .is_err()
        );
        assert!(
            KittyShmProtocol::from_rgba_writer(FontSize::new(0, 3), size, false, |_, _, _| {})
                .is_err()
        );

        let proto = KittyShmProtocol::from_rgba_writer(
            font_size,
            size,
            false,
            |mapped, target_px_w, target_px_h| {
                assert_eq!((target_px_w, target_px_h), (20, 12));
                assert_eq!(mapped.len(), (20 * 12 * 4) as usize);
                // Write a single opaque pixel at (5, 3); leave all other bytes untouched.
                let offset = ((3 * 20 + 5) * 4) as usize;
                mapped[offset..offset + 4].copy_from_slice(&[11, 22, 33, 255]);
            },
        )
        .expect("from_rgba_writer should succeed");

        assert!(proto.transmit_str().contains("t=s"));
        assert!(proto.transmit_str().contains("f=32"));
        assert!(proto.transmit_str().contains("s=20,v=12"));
        assert!(proto.transmit_str().contains("S=960"));

        let bytes = proto.read_shm_bytes();
        let mut expected = vec![0u8; (20 * 12 * 4) as usize];
        let offset = ((3 * 20 + 5) * 4) as usize;
        expected[offset..offset + 4].copy_from_slice(&[11, 22, 33, 255]);
        assert_eq!(bytes, expected);
    }
}
