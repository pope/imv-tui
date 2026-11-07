//! Low-level image file range loading, RAW metadata parsing, and EXIF processing.

use std::io::{Read, Seek};

use crate::imaging::ImageSource;

/// Maximum probe limit in bytes for RAW EXIF metadata and preview extraction (2MB).
/// RAW files often store IFD tags and JPEG preview offsets past 256KB while we
/// still avoid reading 50MB+ sensor payloads into memory.
pub const RAW_EXIF_PROBE_LIMIT: usize = 2 * 1024 * 1024;

/// Known file extensions for RAW camera image containers.
pub const RAW_EXTENSIONS: &[&str] = &["dng", "cr2", "nef", "arw", "orf", "rw2", "pef", "raf"];

/// Returns true if the lowercase file extension corresponds to a RAW camera format.
pub fn is_raw_ext(ext: &str) -> bool {
    RAW_EXTENSIONS
        .iter()
        .any(|&raw_ext| ext.eq_ignore_ascii_case(raw_ext))
}

/// Returns true if `bytes` starts with a TIFF container header:
/// - Little-endian standard: `[b'I', b'I', 0x2A, 0x00]`
/// - Big-endian standard: `[b'M', b'M', 0x00, 0x2A]`
/// - Panasonic RW2: `[b'I', b'I', 0x55, 0x00]`
/// - Olympus ORF: `[b'I', b'I', b'R', b'O']` (`0x4F52`) or `[b'I', b'I', b'R', b'S']` (`0x5352`)
pub fn is_tiff_header(bytes: &[u8]) -> bool {
    if bytes.len() < 4 {
        return false;
    }
    (bytes[0] == 0x49 && bytes[1] == 0x49 && bytes[2] == 0x2A && bytes[3] == 0x00)
        || (bytes[0] == 0x4D && bytes[1] == 0x4D && bytes[2] == 0x00 && bytes[3] == 0x2A)
        || (bytes[0] == 0x49 && bytes[1] == 0x49 && bytes[2] == 0x55 && bytes[3] == 0x00)
        || (bytes[0] == 0x49
            && bytes[1] == 0x49
            && bytes[2] == b'R'
            && (bytes[3] == b'O' || bytes[3] == b'S'))
}

/// Returns true if `bytes` begins with a recognized Camera RAW container header
/// (TIFF-based RAW or Fujifilm RAF).
pub fn is_raw_container_header(bytes: &[u8]) -> bool {
    is_tiff_header(bytes) || bytes.starts_with(b"FUJIFILM")
}

/// Maps a raw EXIF orientation integer value (1..=8) to `image::metadata::Orientation`.
pub const fn orientation_from_exif_value(val: u16) -> image::metadata::Orientation {
    match val {
        2 => image::metadata::Orientation::FlipHorizontal,
        3 => image::metadata::Orientation::Rotate180,
        4 => image::metadata::Orientation::FlipVertical,
        5 => image::metadata::Orientation::Rotate90FlipH,
        6 => image::metadata::Orientation::Rotate90,
        7 => image::metadata::Orientation::Rotate270FlipH,
        8 => image::metadata::Orientation::Rotate270,
        _ => image::metadata::Orientation::NoTransforms,
    }
}

/// Extracts orientation from an EXIF data structure, falling back to `NoTransforms` if absent or malformed.
pub fn get_orientation_from_exif(exif: &exif::Exif) -> image::metadata::Orientation {
    let val = exif
        .get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|f| match f.value {
            exif::Value::Short(ref v) => v.first().copied(),
            _ => None,
        })
        .unwrap_or(1);
    orientation_from_exif_value(val)
}

/// Parses EXIF metadata from `bytes` using tolerant mode (`continue_on_error(true)`),
/// returning partial results even when maker notes or vendor tags are malformed.
pub fn parse_tolerant_exif(bytes: &[u8]) -> Option<exif::Exif> {
    let mut cursor = std::io::Cursor::new(bytes);
    exif::Reader::new()
        .continue_on_error(true)
        .read_from_container(&mut cursor)
        .or_else(|e| e.distill_partial_result(|_| ()))
        .ok()
}

/// Segmented TIFF source for zero-allocation parsing of files where the root
/// IFD0 directory or tags are located at the tail of the file rather than the initial header.
#[derive(Debug, Clone, Copy)]
pub struct TiffSource<'a> {
    header: &'a [u8],
    tail_offset: u64,
    tail: &'a [u8],
}

impl<'a> TiffSource<'a> {
    /// Initializes a TIFF source wrapping a single contiguous header slice.
    pub fn init(header: &'a [u8]) -> Self {
        Self {
            header,
            tail_offset: 0,
            tail: &[],
        }
    }

    /// Initializes a TIFF source wrapping a header slice and a tail slice loaded from `tail_offset`.
    pub fn init_with_tail(header: &'a [u8], tail_offset: u64, tail: &'a [u8]) -> Self {
        Self {
            header,
            tail_offset,
            tail,
        }
    }

    /// Retrieves a contiguous slice of `len` bytes starting at `off` from either header or tail buffer.
    pub fn get_slice(&self, off: usize, len: usize) -> Option<&'a [u8]> {
        if let Some(end) = off.checked_add(len)
            && end <= self.header.len()
        {
            return Some(&self.header[off..end]);
        }
        if !self.tail.is_empty() && (off as u64) >= self.tail_offset {
            let rel_u64 = (off as u64).saturating_sub(self.tail_offset);
            if rel_u64 <= self.tail.len() as u64 {
                let rel = rel_u64 as usize;
                if let Some(tail_end) = rel.checked_add(len)
                    && tail_end <= self.tail.len()
                {
                    return Some(&self.tail[rel..tail_end]);
                }
            }
        }
        None
    }

    /// Retrieves an available prefix slice of up to `max_len` bytes starting at `off` from either header or tail buffer.
    ///
    /// Unlike [`Self::get_slice`], if `off` is within buffer bounds but fewer than `max_len` bytes remain until the end
    /// of the buffer, this returns whatever non-empty slice is available instead of `None`.
    pub fn get_prefix_slice(&self, off: usize, max_len: usize) -> Option<&'a [u8]> {
        if max_len == 0 {
            return None;
        }
        if off < self.header.len() {
            let available = self.header.len() - off;
            let take = available.min(max_len);
            return Some(&self.header[off..off + take]);
        }
        if !self.tail.is_empty() && (off as u64) >= self.tail_offset {
            let rel_u64 = (off as u64).saturating_sub(self.tail_offset);
            if rel_u64 < self.tail.len() as u64 {
                let rel = rel_u64 as usize;
                let available = self.tail.len() - rel;
                let take = available.min(max_len);
                return Some(&self.tail[rel..rel + take]);
            }
        }
        None
    }
}

/// Probes the 8-byte TIFF container header to extract the root IFD0 offset without reading or parsing tags.
pub fn probe_tiff_ifd0_offset(header: &[u8]) -> Option<u32> {
    if header.len() < 8 {
        return None;
    }
    let is_le = if header.starts_with(b"II") {
        true
    } else if header.starts_with(b"MM") {
        false
    } else {
        return None;
    };
    let sig = if is_le {
        u16::from_le_bytes([header[2], header[3]])
    } else {
        u16::from_be_bytes([header[2], header[3]])
    };
    if sig != 0x002A && sig != 0x0055 && sig != 0x4F52 && sig != 0x5352 {
        return None;
    }
    let ifd0_off = if is_le {
        u32::from_le_bytes(header[4..8].try_into().ok()?)
    } else {
        u32::from_be_bytes(header[4..8].try_into().ok()?)
    };
    if ifd0_off < 8 || (ifd0_off & 1) != 0 {
        return None;
    }
    Some(ifd0_off)
}

/// Byte offset and length of an embedded JPEG bitstream inside an image container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PreviewLocation {
    /// Byte offset from the start of the container file.
    pub offset: u64,
    /// Byte length of the embedded JPEG stream.
    pub length: u64,
}

impl PreviewLocation {
    /// Returns `buf[offset .. offset + length]` if non-empty and fits within `buf`, or `None`.
    pub fn slice_in_bounds<'a>(&self, buf: &'a [u8]) -> Option<&'a [u8]> {
        if self.length == 0 {
            return None;
        }
        let end = self.offset.checked_add(self.length)?;
        if end <= buf.len() as u64 {
            Some(&buf[self.offset as usize..end as usize])
        } else {
            None
        }
    }

    /// Returns `true` when `length > 0` and within `max_bytes` (if specified).
    pub fn is_valid(&self, max_bytes: Option<u64>) -> bool {
        if self.length == 0 {
            return false;
        }
        if let Some(max) = max_bytes
            && self.length > max
        {
            return false;
        }
        self.offset.checked_add(self.length).is_some()
    }
}

/// Unified header-probe result for Camera RAW and TIFF/EXIF containers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawHeaderProbe {
    /// Largest embedded JPEG preview location.
    pub preview: Option<PreviewLocation>,
    /// Intermediate-resolution JPEG preview location (<= 2MB), if distinct from `preview` and `thumbnail`.
    pub medium: Option<PreviewLocation>,
    /// Smallest embedded JPEG thumbnail location (<= 512KB).
    pub thumbnail: Option<PreviewLocation>,
    /// All discovered embedded JPEG candidate locations sorted by length descending (largest first).
    pub candidates_desc: Vec<PreviewLocation>,
    /// Unoriented sensor/image pixel dimensions `(width, height)`.
    pub sensor_dims: Option<(u32, u32)>,
    /// EXIF orientation transform for the container.
    pub orientation: image::metadata::Orientation,
}

impl Default for RawHeaderProbe {
    fn default() -> Self {
        Self {
            preview: None,
            medium: None,
            thumbnail: None,
            candidates_desc: Vec::new(),
            sensor_dims: None,
            orientation: image::metadata::Orientation::NoTransforms,
        }
    }
}

/// Reads bytes for `loc` either by slicing `header_bytes` if fully within bounds,
/// or by reading from `source` via `read_source_range`.
pub fn read_location_bytes(
    header_bytes: &[u8],
    source: Option<&ImageSource>,
    loc: PreviewLocation,
) -> Option<Vec<u8>> {
    if !loc.is_valid(None) {
        return None;
    }
    if let Some(slice) = loc.slice_in_bounds(header_bytes) {
        return Some(slice.to_vec());
    }
    if let Some(src) = source {
        return read_source_range(src, loc.offset, loc.length).ok();
    }
    None
}

/// Reads the source raw bytes from local files or CBZ zip archives, reusing `header_bytes` if supplied for RAW files.
pub fn read_source_bytes_with_header(
    source: &ImageSource,
    header_bytes: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    if source.is_raw() {
        let preview = match header_bytes {
            Some(hb) => read_raw_preview_bytes_with_header(source, hb),
            None => read_raw_preview_bytes(source),
        };
        return match preview {
            Some(preview_bytes) => Ok(preview_bytes),
            None => Err(format!(
                "Failed to extract embedded JPEG preview from RAW file:\n{}",
                source.display_name()
            )),
        };
    }

    match source {
        ImageSource::Local(path) => std::fs::read(path)
            .map_err(|e| format!("Failed to read file:\n{}\n\nError: {}", path.display(), e)),
        ImageSource::Cbz {
            zip_path,
            file_in_zip,
        } => {
            let file = std::fs::File::open(zip_path)
                .map_err(|e| format!("Failed to open zip file {}: {}", zip_path.display(), e))?;
            let reader = std::io::BufReader::new(file);
            let mut archive = zip::ZipArchive::new(reader)
                .map_err(|e| format!("Failed to read zip archive {}: {}", zip_path.display(), e))?;
            let mut zip_entry = archive.by_name(file_in_zip).map_err(|e| {
                format!(
                    "Failed to locate page {} in {}: {}",
                    file_in_zip,
                    zip_path.display(),
                    e
                )
            })?;
            let mut buffer = Vec::with_capacity(zip_entry.size() as usize);
            zip_entry.read_to_end(&mut buffer).map_err(|e| {
                format!(
                    "Failed to read page data {} from {}: {}",
                    file_in_zip,
                    zip_path.display(),
                    e
                )
            })?;
            Ok(buffer)
        }
    }
}

/// Reads the source raw bytes from local files or CBZ zip archives.
#[cfg(test)]
pub fn read_source_bytes(source: &ImageSource) -> Result<Vec<u8>, String> {
    read_source_bytes_with_header(source, None)
}

/// Reads only the first `limit` bytes from local files or CBZ archives.
/// This prevents loading massive files fully into memory when only the EXIF header or thumbnail is needed.
pub fn read_source_bytes_limited(source: &ImageSource, limit: usize) -> Result<Vec<u8>, String> {
    match source {
        ImageSource::Local(path) => {
            let file = std::fs::File::open(path)
                .map_err(|e| format!("Failed to open file:\n{}\n\nError: {}", path.display(), e))?;
            let mut buffer = Vec::new();
            file.take(limit as u64)
                .read_to_end(&mut buffer)
                .map_err(|e| format!("Failed to read file:\n{}\n\nError: {}", path.display(), e))?;
            Ok(buffer)
        }
        ImageSource::Cbz {
            zip_path,
            file_in_zip,
        } => {
            let file = std::fs::File::open(zip_path)
                .map_err(|e| format!("Failed to open zip file {}: {}", zip_path.display(), e))?;
            let reader = std::io::BufReader::new(file);
            let mut archive = zip::ZipArchive::new(reader)
                .map_err(|e| format!("Failed to read zip archive {}: {}", zip_path.display(), e))?;
            let zip_entry = archive.by_name(file_in_zip).map_err(|e| {
                format!(
                    "Failed to locate page {} in {}: {}",
                    file_in_zip,
                    zip_path.display(),
                    e
                )
            })?;
            let mut buffer = Vec::new();
            zip_entry
                .take(limit as u64)
                .read_to_end(&mut buffer)
                .map_err(|e| {
                    format!(
                        "Failed to read page data {} from {}: {}",
                        file_in_zip,
                        zip_path.display(),
                        e
                    )
                })?;
            Ok(buffer)
        }
    }
}

/// Reads a specific range of bytes from the image source.
pub fn read_source_range(
    source: &ImageSource,
    offset: u64,
    length: u64,
) -> Result<Vec<u8>, String> {
    if length > 64 * 1024 * 1024 {
        return Err(format!(
            "Requested read segment length {} is too large (max 64MB)",
            length
        ));
    }
    match source {
        ImageSource::Local(path) => {
            let mut file =
                std::fs::File::open(path).map_err(|e| format!("Failed to open file: {}", e))?;
            file.seek(std::io::SeekFrom::Start(offset))
                .map_err(|e| format!("Failed to seek file: {}", e))?;
            let mut buffer = vec![0u8; length as usize];
            file.read_exact(&mut buffer)
                .map_err(|e| format!("Failed to read file segment: {}", e))?;
            Ok(buffer)
        }
        ImageSource::Cbz {
            zip_path,
            file_in_zip,
        } => {
            let file = std::fs::File::open(zip_path)
                .map_err(|e| format!("Failed to open zip file: {}", e))?;
            let reader = std::io::BufReader::new(file);
            let mut archive = zip::ZipArchive::new(reader)
                .map_err(|e| format!("Failed to read zip archive: {}", e))?;
            let mut zip_entry = archive
                .by_name(file_in_zip)
                .map_err(|e| format!("Failed to locate zip entry: {}", e))?;
            let _total_len = offset
                .checked_add(length)
                .ok_or_else(|| "Offset + length calculation overflowed u64".to_string())?;
            std::io::copy(&mut zip_entry.by_ref().take(offset), &mut std::io::sink())
                .map_err(|e| format!("Failed to seek zip entry offset: {}", e))?;
            let mut buffer = vec![0u8; length as usize];
            zip_entry
                .read_exact(&mut buffer)
                .map_err(|e| format!("Failed to read zip entry segment: {}", e))?;
            Ok(buffer)
        }
    }
}

/// Reads a bounded segment from `source` starting at `offset` up to `max_length` bytes,
/// without failing on early EOF if fewer bytes are available.
pub fn read_source_range_up_to(
    source: &ImageSource,
    offset: u64,
    max_length: usize,
) -> Result<Vec<u8>, String> {
    if max_length > 64 * 1024 * 1024 {
        return Err(format!(
            "Requested read segment length {} is too large (max 64MB)",
            max_length
        ));
    }
    match source {
        ImageSource::Local(path) => {
            let mut file =
                std::fs::File::open(path).map_err(|e| format!("Failed to open file: {}", e))?;
            file.seek(std::io::SeekFrom::Start(offset))
                .map_err(|e| format!("Failed to seek file: {}", e))?;
            let mut buffer = Vec::new();
            file.take(max_length as u64)
                .read_to_end(&mut buffer)
                .map_err(|e| format!("Failed to read file segment: {}", e))?;
            Ok(buffer)
        }
        ImageSource::Cbz {
            zip_path,
            file_in_zip,
        } => {
            let file = std::fs::File::open(zip_path)
                .map_err(|e| format!("Failed to open zip file: {}", e))?;
            let reader = std::io::BufReader::new(file);
            let mut archive = zip::ZipArchive::new(reader)
                .map_err(|e| format!("Failed to read zip archive: {}", e))?;
            let mut zip_entry = archive
                .by_name(file_in_zip)
                .map_err(|e| format!("Failed to locate zip entry: {}", e))?;
            std::io::copy(&mut zip_entry.by_ref().take(offset), &mut std::io::sink())
                .map_err(|e| format!("Failed to seek zip entry offset: {}", e))?;
            let mut buffer = Vec::new();
            zip_entry
                .take(max_length as u64)
                .read_to_end(&mut buffer)
                .map_err(|e| format!("Failed to read zip entry segment: {}", e))?;
            Ok(buffer)
        }
    }
}

/// Returns `true` if `bytes` begins with a valid JPEG SOI header and contains a standard
/// baseline/extended/progressive DCT frame (`SOF0`..=`SOF2`), rejecting lossless JPEG (`SOF3`)
/// raw sensor streams that standard JPEG decoders cannot decode.
pub fn is_decodable_jpeg(bytes: &[u8]) -> bool {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 || bytes[2] != 0xFF {
        return false;
    }
    let mut i = 2;
    let limit = bytes.len().min(64 * 1024);
    while i + 1 < limit {
        if bytes[i] != 0xFF {
            i += 1;
            continue;
        }
        while i < limit && bytes[i] == 0xFF {
            i += 1;
        }
        if i >= limit {
            break;
        }
        let marker = bytes[i];
        i += 1;
        match marker {
            // Baseline (SOF0), Extended Sequential (SOF1), Progressive (SOF2) DCT
            0xC0..=0xC2 => return true,
            // Lossless JPEG (SOF3) and differential/arithmetic lossless variants -> reject!
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => return false,
            // SOS (Start of Scan) or EOI (End of Image)
            0xDA | 0xD9 => break,
            // Standalone markers without length payload
            0x00 | 0x01 | 0xD0..=0xD8 => continue,
            _ => {
                if i + 2 > limit {
                    break;
                }
                let seg_len = u16::from_be_bytes([bytes[i], bytes[i + 1]]) as usize;
                if seg_len < 2 {
                    return false;
                }
                i = i.saturating_add(seg_len);
            }
        }
    }
    // If the slice was truncated before reaching SOF, accept if it has a valid SOI and no SOF3 was seen
    true
}

fn extract_tag_pair(
    exif: &exif::Exif,
    tag_off: exif::Tag,
    tag_len: exif::Tag,
    ifd: exif::In,
) -> Option<(u64, u64)> {
    let f_off = exif.get_field(tag_off, ifd)?;
    let f_len = exif.get_field(tag_len, ifd)?;

    let off = match f_off.value {
        exif::Value::Long(ref v) => v.first().copied(),
        exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
        _ => None,
    }?;
    let len = match f_len.value {
        exif::Value::Long(ref v) => v.first().copied(),
        exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
        _ => None,
    }?;

    if len > 0 {
        Some((off as u64, len as u64))
    } else {
        None
    }
}

/// Collects byte offset and length pairs of all embedded JPEG stream candidates
/// (`JPEGInterchangeFormat` or JPEG-compressed `StripOffsets`) across all IFDs in `exif`.
pub fn collect_exif_jpeg_candidates(exif: &exif::Exif) -> Vec<(u64, u64)> {
    let mut ifds: Vec<exif::In> = vec![exif::In::PRIMARY, exif::In::THUMBNAIL];
    for f in exif.fields() {
        if !ifds.contains(&f.ifd_num) {
            ifds.push(f.ifd_num);
        }
    }

    let mut candidates = Vec::new();
    for ifd in ifds {
        // 1. JPEGInterchangeFormat / JPEGInterchangeFormatLength
        if let Some(pair) = extract_tag_pair(
            exif,
            exif::Tag::JPEGInterchangeFormat,
            exif::Tag::JPEGInterchangeFormatLength,
            ifd,
        ) {
            candidates.push(pair);
        }

        // 2. StripOffsets / StripByteCounts when Compression is absent or 6/7 (JPEG)
        let is_jpeg_compression = match exif.get_field(exif::Tag::Compression, ifd) {
            Some(f) => match f.value {
                exif::Value::Short(ref v) => v.first().copied().is_some_and(|c| c == 6 || c == 7),
                exif::Value::Long(ref v) => v.first().copied().is_some_and(|c| c == 6 || c == 7),
                _ => false,
            },
            None => true,
        };

        if is_jpeg_compression
            && let Some(pair) = extract_tag_pair(
                exif,
                exif::Tag::StripOffsets,
                exif::Tag::StripByteCounts,
                ifd,
            )
        {
            candidates.push(pair);
        }
    }

    candidates.sort();
    candidates.dedup();
    candidates
}

#[inline]
fn read_u16(bytes: &[u8], is_le: bool) -> u16 {
    if is_le {
        u16::from_le_bytes([bytes[0], bytes[1]])
    } else {
        u16::from_be_bytes([bytes[0], bytes[1]])
    }
}

#[inline]
fn read_u32(bytes: &[u8], is_le: bool) -> u32 {
    if is_le {
        u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    } else {
        u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
    }
}

fn extract_dng_private_preview(
    s: TiffSource<'_>,
    val_or_off: u32,
    val_count: usize,
) -> Option<PreviewLocation> {
    if val_count < 70 {
        return None;
    }
    let p_slice = s.get_slice(val_or_off as usize, 70)?;
    if !p_slice.starts_with(b"RICOH\0II") && !p_slice.starts_with(b"PENTAX\0II") {
        return None;
    }
    // MakerNote explicitly asserts "II" (Little Endian)
    let p_len = u32::from_le_bytes(p_slice[54..58].try_into().ok()?);
    let p_rel_off = u32::from_le_bytes(p_slice[66..70].try_into().ok()?);
    let is_valid_len = p_len > 0 && p_len < 512 * 1024;
    let is_in_bounds = (p_rel_off as u64) + (p_len as u64) <= val_count as u64;
    if !is_valid_len || !is_in_bounds {
        return None;
    }
    let mn_jpeg_off = val_or_off.checked_add(p_rel_off)?;
    Some(PreviewLocation {
        offset: mn_jpeg_off as u64,
        length: p_len as u64,
    })
}

fn collect_sub_ifd_offsets(
    s: TiffSource<'_>,
    val_or_ptr: u32,
    count: usize,
    type_id: u16,
    is_le: bool,
    sub_ifds: &mut Vec<u32>,
) {
    if count == 1 {
        if val_or_ptr > 0 && !sub_ifds.contains(&val_or_ptr) && sub_ifds.len() < 8 {
            sub_ifds.push(val_or_ptr);
        }
    } else if count > 1 && (type_id == 4 || type_id == 13) {
        let fetch_count = count.min(8);
        for i in 0..fetch_count {
            if sub_ifds.len() >= 8 {
                break;
            }
            let read_pos = match (val_or_ptr as usize).checked_add(i * 4) {
                Some(pos) => pos,
                None => break,
            };
            if let Some(s_slice) = s.get_slice(read_pos, 4) {
                let s_off = read_u32(s_slice, is_le);
                if s_off > 0 && !sub_ifds.contains(&s_off) {
                    sub_ifds.push(s_off);
                }
            }
        }
    }
}

fn find_tiff_payload<'a>(source: TiffSource<'a>) -> Option<(TiffSource<'a>, bool, u32, u64)> {
    // 1. Direct TIFF container (Sony ARW, DNG, CR2, NEF, ORF, RW2, PEF, etc.)
    if let Some(h8) = source.get_slice(0, 8)
        && (h8.starts_with(b"II") || h8.starts_with(b"MM"))
    {
        let is_le = h8[0] == b'I';
        let sig = read_u16(&h8[2..4], is_le);
        if sig == 0x002A || sig == 0x0055 || sig == 0x4F52 || sig == 0x5352 {
            let ifd0_off = read_u32(&h8[4..8], is_le);
            if ifd0_off >= 8
                && (ifd0_off & 1) == 0
                && source.get_slice(ifd0_off as usize, 2).is_some()
            {
                return Some((source, is_le, ifd0_off, 0));
            }
        }
    }

    // 2. JPEG APP1 EXIF segment
    if source.header.len() < 14 || source.header[0] != 0xFF || source.header[1] != 0xD8 {
        return None;
    }

    let mut idx = 2;
    while idx + 4 <= source.header.len() {
        if source.header[idx] != 0xFF {
            break;
        }
        let mut marker_offset = idx + 1;
        while marker_offset < source.header.len() && source.header[marker_offset] == 0xFF {
            marker_offset += 1;
        }
        if marker_offset >= source.header.len() {
            break;
        }
        let marker = source.header[marker_offset];
        idx = marker_offset + 1;

        if marker == 0xD9 || marker == 0xDA {
            break;
        }

        if idx + 2 > source.header.len() {
            break;
        }
        let seg_len = ((source.header[idx] as usize) << 8) | (source.header[idx + 1] as usize);
        if seg_len < 2 {
            break;
        }

        let available_len = seg_len.min(source.header.len() - idx);
        let payload = &source.header[idx + 2..idx + available_len];

        if marker == 0xE1 && payload.len() >= 14 && payload.starts_with(b"Exif\0\0") {
            let tiff = &payload[6..];
            if tiff.len() >= 8 {
                let is_le = if tiff.starts_with(b"II") {
                    true
                } else if tiff.starts_with(b"MM") {
                    false
                } else {
                    idx += seg_len;
                    continue;
                };

                let sig = read_u16(&tiff[2..4], is_le);
                if sig == 0x002A || sig == 0x0055 || sig == 0x4F52 || sig == 0x5352 {
                    let ifd0_off = read_u32(&tiff[4..8], is_le);
                    if ifd0_off >= 8 && (ifd0_off & 1) == 0 && (ifd0_off as usize) + 2 <= tiff.len()
                    {
                        let tiff_start_off =
                            (tiff.as_ptr() as usize - source.header.as_ptr() as usize) as u64;
                        return Some((TiffSource::init(tiff), is_le, ifd0_off, tiff_start_off));
                    }
                }
            }
        }

        if idx + seg_len > source.header.len() {
            break;
        }
        idx += seg_len;
    }

    None
}

/// Locates the byte offset and length of an embedded JPEG thumbnail in IFD1 without allocating memory.
pub fn extract_jpeg_thumbnail_location(data: &[u8]) -> Option<PreviewLocation> {
    let (tiff_source, is_le, ifd0_off, tiff_start_off) = find_tiff_payload(TiffSource::init(data))?;
    let tiff = tiff_source.header;
    if (ifd0_off as usize) + 2 > tiff.len() {
        return None;
    }
    let num_ifd0_entries =
        read_u16(&tiff[ifd0_off as usize..ifd0_off as usize + 2], is_le) as usize;
    let ifd1_ptr_off = (ifd0_off as usize).checked_add(2 + num_ifd0_entries * 12)?;
    if ifd1_ptr_off + 4 > tiff.len() {
        return None;
    }

    let ifd1_off = read_u32(&tiff[ifd1_ptr_off..ifd1_ptr_off + 4], is_le) as usize;
    if ifd1_off == 0 || ifd1_off + 2 > tiff.len() {
        return None;
    }

    let num_ifd1_entries = read_u16(&tiff[ifd1_off..ifd1_off + 2], is_le) as usize;
    let mut entry_off = ifd1_off + 2;

    let mut j_off = None;
    let mut j_len = None;
    let mut strip_off = None;
    let mut strip_len = None;

    for _ in 0..num_ifd1_entries {
        if entry_off + 12 > tiff.len() {
            break;
        }
        let entry = &tiff[entry_off..entry_off + 12];
        entry_off += 12;

        let tag = read_u16(&entry[0..2], is_le);
        let type_id = read_u16(&entry[2..4], is_le);
        let val = if type_id == 3 {
            read_u16(&entry[8..10], is_le) as u32
        } else {
            read_u32(&entry[8..12], is_le)
        };

        match tag {
            0x0201 => j_off = Some(val),
            0x0202 => j_len = Some(val),
            0x0111 => strip_off = Some(val),
            0x0117 => strip_len = Some(val),
            _ => {}
        }
    }

    if let (Some(off), Some(len)) = (j_off, j_len)
        && off > 0
        && len > 0
    {
        return Some(PreviewLocation {
            offset: tiff_start_off.checked_add(off as u64)?,
            length: len as u64,
        });
    }

    if let (Some(off), Some(len)) = (strip_off, strip_len)
        && off > 0
        && len > 0
    {
        return Some(PreviewLocation {
            offset: tiff_start_off.checked_add(off as u64)?,
            length: len as u64,
        });
    }

    None
}

/// Probes a TIFF-based RAW container or EXIF APP1 header for preview candidates, sensor dimensions, and orientation.
pub fn probe_tiff_raw_header(header: &[u8], source: Option<&ImageSource>) -> RawHeaderProbe {
    let mut tail_buf = Vec::new();
    if let Some(ifd0_off) = probe_tiff_ifd0_offset(header)
        && ifd0_off as usize >= header.len()
        && let Some(src) = source
        && let Ok(buf) = read_source_range_up_to(src, ifd0_off as u64, 64 * 1024)
    {
        tail_buf = buf;
    }

    let tiff_source = if !tail_buf.is_empty() {
        if let Some(ifd0_off) = probe_tiff_ifd0_offset(header) {
            TiffSource::init_with_tail(header, ifd0_off as u64, &tail_buf)
        } else {
            TiffSource::init(header)
        }
    } else {
        TiffSource::init(header)
    };

    let (src, is_le, ifd0_off, tiff_start_off) = match find_tiff_payload(tiff_source) {
        Some(t) => t,
        None => return RawHeaderProbe::default(),
    };

    let sig = if let Some(h4) = src.get_slice(2, 2) {
        read_u16(h4, is_le)
    } else {
        0
    };
    let is_rw2 = sig == 0x0055;

    // Detect Canon CR2 container signature at bytes 8..12 ("CR\x02\x00")
    let is_cr2 = src.get_slice(8, 4).is_some_and(|s| s == b"CR\x02\x00");

    let mut candidates = Vec::new();
    let mut best_visible_dims: Option<((u32, u32), u64)> = None;
    let mut best_cfa_dims: Option<((u32, u32), u64)> = None;
    let mut rw2_sensor_w = None;
    let mut rw2_sensor_h = None;
    let mut orientation_val = None;
    let mut sub_ifd_list = Vec::new();
    let mut exif_ifd_list = Vec::new();

    let parse_ifd = |off: u32,
                     depth: usize,
                     is_subifd: bool,
                     cands: &mut Vec<PreviewLocation>,
                     sub_ifds: &mut Vec<u32>,
                     exif_ifds: &mut Vec<u32>,
                     best_vis: &mut Option<((u32, u32), u64)>,
                     best_cfa: &mut Option<((u32, u32), u64)>,
                     rw2_w: &mut Option<u32>,
                     rw2_h: &mut Option<u32>,
                     orient: &mut Option<u16>|
     -> u32 {
        if off < 8 || (off & 1) != 0 {
            return 0;
        }
        let num_bytes = match src.get_slice(off as usize, 2) {
            Some(b) => b,
            None => return 0,
        };
        let num = read_u16(num_bytes, is_le) as usize;
        let mut entry_off = (off as usize) + 2;

        let mut cur_w = None;
        let mut cur_h = None;
        let mut subfile_type = None;
        let mut photometric = None;
        let mut compression = None;
        let mut planar_config = None;
        let mut j_off = None;
        let mut j_len = None;
        let mut strip_off = None;
        let mut strip_len = None;
        let mut is_cr2_raw_ifd = false;

        for _ in 0..num {
            let entry = match src.get_slice(entry_off, 12) {
                Some(e) => e,
                None => break,
            };
            entry_off += 12;

            let tag = read_u16(&entry[0..2], is_le);
            let type_id = read_u16(&entry[2..4], is_le);
            let val_count = read_u32(&entry[4..8], is_le);
            let val_or_off = read_u32(&entry[8..12], is_le);
            let val = if type_id == 3 {
                read_u16(&entry[8..10], is_le) as u32
            } else {
                val_or_off
            };

            match tag {
                0x00FE => subfile_type = Some(val),
                0x0100 | 0xA002 => cur_w = Some(val),
                0x0101 | 0xA003 => cur_h = Some(val),
                0x0102 => {}
                0x0103 => compression = Some(val as u16),
                0x0106 => photometric = Some(val as u16),
                0x0111 => {
                    if val_count == 1 {
                        strip_off = Some(if type_id == 3 {
                            read_u16(&entry[8..10], is_le) as u32
                        } else {
                            val_or_off
                        });
                    }
                }
                0x0112 => {
                    if orient.is_none() {
                        *orient = Some(val as u16);
                    }
                }
                0x0117 => {
                    if val_count == 1 {
                        strip_len = Some(if type_id == 3 {
                            read_u16(&entry[8..10], is_le) as u32
                        } else {
                            val_or_off
                        });
                    }
                }
                0x011C => planar_config = Some(val as u16),
                0x014A => {
                    collect_sub_ifd_offsets(
                        src,
                        val_or_off,
                        val_count as usize,
                        type_id,
                        is_le,
                        sub_ifds,
                    );
                }
                0x0201 => j_off = Some(val_or_off),
                0x0202 => j_len = Some(val_or_off),
                0x8769 => {
                    if val_or_off > 0 && !exif_ifds.contains(&val_or_off) {
                        exif_ifds.push(val_or_off);
                    }
                }
                0xC5D8 | 0xC5D9 | 0xC5E0 | 0xC640 => {
                    is_cr2_raw_ifd = true;
                }
                0xC634 => {
                    if let Some(cand) =
                        extract_dng_private_preview(src, val_or_off, val_count as usize)
                    {
                        cands.push(cand);
                    }
                }
                0x002E if is_rw2 => {
                    if val_or_off > 0 && val_count > 4 {
                        cands.push(PreviewLocation {
                            offset: val_or_off as u64,
                            length: val_count as u64,
                        });
                    }
                }
                0x0002 if is_rw2 => {
                    *rw2_w = Some(val);
                }
                0x0003 if is_rw2 => {
                    *rw2_h = Some(val);
                }
                _ => {}
            }
        }

        // 1. JPEGInterchangeFormat candidate
        if let (Some(jo), Some(jl)) = (j_off, j_len)
            && jo > 0
            && jl > 0
            && (!is_cr2 || depth < 2)
            && !is_cr2_raw_ifd
        {
            cands.push(PreviewLocation {
                offset: tiff_start_off.saturating_add(jo as u64),
                length: jl as u64,
            });
        }

        // 2. StripOffsets candidate (independent if, NOT else if)
        if let (Some(so), Some(sl)) = (strip_off, strip_len)
            && so > 0
            && sl > 0
            && (!is_cr2 || depth < 2)
            && !is_cr2_raw_ifd
            && planar_config != Some(2)
        {
            let is_jpeg_comp =
                compression.is_none() || compression == Some(6) || compression == Some(7);
            let is_jpeg_color = photometric.is_none() || matches!(photometric, Some(1 | 2 | 6));
            let is_bayer_color = matches!(photometric, Some(32803 | 34892));
            let is_reduced_or_unspecified = subfile_type.is_none()
                || matches!(subfile_type, Some(s) if (s & 1) != 0 || s == 0x00010000);
            let subfile_accepted = is_reduced_or_unspecified
                || (!is_bayer_color && (compression == Some(6) || compression == Some(7)));

            let is_decodable = if let Some(strip_bytes) =
                src.get_prefix_slice(so as usize, (sl as usize).min(64 * 1024))
            {
                is_decodable_jpeg(strip_bytes)
            } else if let Some(soi) = src.get_slice(so as usize, 2) {
                soi == [0xFF, 0xD8]
            } else {
                true
            };

            if is_jpeg_comp && is_jpeg_color && !is_bayer_color && subfile_accepted && is_decodable
            {
                cands.push(PreviewLocation {
                    offset: tiff_start_off.saturating_add(so as u64),
                    length: sl as u64,
                });
            }
        }

        // Dimensions
        if let (Some(w), Some(h)) = (cur_w, cur_h)
            && w > 0
            && h > 0
        {
            let is_cr2_raw = is_cr2 && depth >= 2;
            if !is_cr2_raw && !is_cr2_raw_ifd {
                let is_raw_cfa = matches!(photometric, Some(32803 | 34892));
                let is_subifd_raw =
                    is_subifd && matches!(subfile_type, Some(s) if (s & 1) == 0 && s != 0x00010000);
                let area = (w as u64) * (h as u64);
                if is_raw_cfa || is_subifd_raw {
                    if best_cfa.is_none_or(|(_, a)| area > a) {
                        *best_cfa = Some(((w, h), area));
                    }
                } else if best_vis.is_none_or(|(_, a)| area > a) {
                    *best_vis = Some(((w, h), area));
                }
            }
        }

        src.get_slice(entry_off, 4)
            .map(|s| read_u32(s, is_le))
            .unwrap_or(0)
    };

    // 1. Follow IFD chain: IFD0 -> IFD1 -> IFD2 -> ...
    let mut cur_ifd_off = ifd0_off;
    let mut ifd_depth = 0;
    let mut visited = [0u32; 16];

    while cur_ifd_off > 0 && ifd_depth < 8 {
        if visited[..ifd_depth].contains(&cur_ifd_off) {
            break;
        }
        visited[ifd_depth] = cur_ifd_off;

        let next_off = parse_ifd(
            cur_ifd_off,
            ifd_depth,
            false,
            &mut candidates,
            &mut sub_ifd_list,
            &mut exif_ifd_list,
            &mut best_visible_dims,
            &mut best_cfa_dims,
            &mut rw2_sensor_w,
            &mut rw2_sensor_h,
            &mut orientation_val,
        );
        cur_ifd_off = next_off;
        ifd_depth += 1;
    }

    // 2. Parse SubIFDs discovered in IFD0
    for s_off in sub_ifd_list {
        let mut cur_s = s_off;
        let mut s_depth = 1;
        while cur_s > 0 && s_depth < 8 {
            if visited[..ifd_depth].contains(&cur_s) {
                break;
            }
            if ifd_depth < visited.len() {
                visited[ifd_depth] = cur_s;
                ifd_depth += 1;
            }
            let mut dummy_sub = Vec::new();
            let next_s = parse_ifd(
                cur_s,
                s_depth,
                true,
                &mut candidates,
                &mut dummy_sub,
                &mut exif_ifd_list,
                &mut best_visible_dims,
                &mut best_cfa_dims,
                &mut rw2_sensor_w,
                &mut rw2_sensor_h,
                &mut orientation_val,
            );
            cur_s = next_s;
            s_depth += 1;
        }
    }

    // 3. Parse ExifIFDs discovered in IFD0 or SubIFDs
    for e_off in exif_ifd_list {
        if !visited[..ifd_depth].contains(&e_off) {
            if ifd_depth < visited.len() {
                visited[ifd_depth] = e_off;
                ifd_depth += 1;
            }
            let mut dummy_sub = Vec::new();
            let mut dummy_exif = Vec::new();
            parse_ifd(
                e_off,
                1,
                true,
                &mut candidates,
                &mut dummy_sub,
                &mut dummy_exif,
                &mut best_visible_dims,
                &mut best_cfa_dims,
                &mut rw2_sensor_w,
                &mut rw2_sensor_h,
                &mut orientation_val,
            );
        }
    }

    // Merge candidates from parse_tolerant_exif
    if let Some(exif) = parse_tolerant_exif(header) {
        for (off, len) in collect_exif_jpeg_candidates(&exif) {
            let abs_off = tiff_start_off.saturating_add(off);
            let loc = PreviewLocation {
                offset: abs_off,
                length: len,
            };
            if !candidates.contains(&loc) {
                let mut is_decodable = true;
                if let Some(slice) = src.get_slice(off as usize, (len as usize).min(64 * 1024))
                    && !is_decodable_jpeg(slice)
                {
                    is_decodable = false;
                }
                if is_decodable {
                    candidates.push(loc);
                }
            }
        }
    }

    // Filter candidates against is_decodable_jpeg on accessible prefix
    candidates.retain(|c| {
        let rel_off = c.offset.saturating_sub(tiff_start_off);
        if let Some(slice) =
            src.get_prefix_slice(rel_off as usize, (c.length as usize).min(64 * 1024))
        {
            is_decodable_jpeg(slice)
        } else {
            true
        }
    });

    candidates.sort_by(|a, b| {
        b.length
            .cmp(&a.length)
            .then_with(|| a.offset.cmp(&b.offset))
    });
    candidates.dedup();

    let preview = candidates.first().copied();
    let thumbnail = candidates
        .iter()
        .rev()
        .copied()
        .find(|c| c.length <= 512 * 1024);

    let medium = candidates
        .iter()
        .copied()
        .find(|c| Some(*c) != preview && Some(*c) != thumbnail && c.length <= 2 * 1024 * 1024);

    let sensor_dims = best_visible_dims
        .map(|(d, _)| d)
        .or_else(|| best_cfa_dims.map(|(d, _)| d))
        .or_else(|| rw2_sensor_w.zip(rw2_sensor_h))
        .or_else(|| parse_tolerant_exif(header).and_then(|e| get_dimensions_from_exif(&e)));

    let orientation = orientation_val
        .map(orientation_from_exif_value)
        .unwrap_or_else(|| get_exif_orientation(header));

    RawHeaderProbe {
        preview,
        medium,
        thumbnail,
        candidates_desc: candidates,
        sensor_dims,
        orientation,
    }
}

/// Reads image pixel dimensions from EXIF metadata or scans JPEG Start-of-Frame (SOF) markers,
/// explicitly skipping `APP1` (`0xFFE1`) to prevent matching embedded thumbnails.
pub fn read_jpeg_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    if let Some((w, h)) = probe_tiff_raw_header(data, None).sensor_dims {
        return Some((w, h));
    }
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return None;
    }

    let mut idx = 2;
    while idx + 4 <= data.len() {
        if data[idx] != 0xFF {
            break;
        }
        let mut marker_offset = idx + 1;
        while marker_offset < data.len() && data[marker_offset] == 0xFF {
            marker_offset += 1;
        }
        if marker_offset >= data.len() {
            break;
        }
        let marker = data[marker_offset];
        idx = marker_offset + 1;

        if marker == 0xD9 || marker == 0xDA {
            break;
        }

        if idx + 2 > data.len() {
            break;
        }
        let seg_len = ((data[idx] as usize) << 8) | (data[idx + 1] as usize);
        if seg_len < 2 || idx + seg_len > data.len() {
            break;
        }

        // Explicitly skip APP1 (0xE1) so we don't match the embedded thumbnail's SOF marker
        if marker == 0xE1 {
            idx += seg_len;
            continue;
        }

        if matches!(
            marker,
            0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF
        ) && idx + 7 <= data.len()
        {
            let h = ((data[idx + 3] as u32) << 8) | (data[idx + 4] as u32);
            let w = ((data[idx + 5] as u32) << 8) | (data[idx + 6] as u32);
            if w > 0 && h > 0 {
                return Some((w, h));
            }
        }

        idx += seg_len;
    }

    None
}

/// Extracts the original full raw image dimensions from the Fujifilm RAF metadata records container bytes.
pub fn get_fuji_raf_dimensions(meta_bytes: &[u8]) -> Option<(u32, u32)> {
    if meta_bytes.len() < 4 {
        return None;
    }
    let mut pos = 0;
    let num_records = match meta_bytes[pos..pos + 4].try_into() {
        Ok(arr) => u32::from_be_bytes(arr) as usize,
        Err(_) => return None,
    };
    pos += 4;

    let mut cropped_size = None;
    let mut image_size = None;
    let mut full_size = None;

    for _ in 0..num_records {
        if pos + 4 > meta_bytes.len() {
            break;
        }
        let tag_id = match meta_bytes[pos..pos + 2].try_into() {
            Ok(arr) => u16::from_be_bytes(arr),
            Err(_) => break,
        };
        let tag_len = match meta_bytes[pos + 2..pos + 4].try_into() {
            Ok(arr) => u16::from_be_bytes(arr) as usize,
            Err(_) => break,
        };
        pos += 4;

        let end = match pos.checked_add(tag_len) {
            Some(end) if end <= meta_bytes.len() => end,
            _ => break,
        };
        let tag_data = &meta_bytes[pos..end];
        pos = end;

        if tag_len >= 4 {
            let h = match tag_data[0..2].try_into() {
                Ok(arr) => u16::from_be_bytes(arr) as u32,
                Err(_) => continue,
            };
            let w = match tag_data[2..4].try_into() {
                Ok(arr) => u16::from_be_bytes(arr) as u32,
                Err(_) => continue,
            };
            if w > 0 && h > 0 {
                if tag_id == 0x0111 {
                    cropped_size = Some((w, h));
                } else if tag_id == 0x0121 {
                    image_size = Some((w, h));
                } else if tag_id == 0x0100 {
                    full_size = Some((w, h));
                }
            }
        }
    }

    cropped_size.or(image_size).or(full_size)
}

/// Probes a Fujifilm `.RAF` container for embedded preview JPEG, nested thumbnail, dimensions, and orientation.
pub fn probe_fuji_raf_header(header_bytes: &[u8], source: Option<&ImageSource>) -> RawHeaderProbe {
    if header_bytes.len() < 100 || !header_bytes.starts_with(b"FUJIFILM") {
        return RawHeaderProbe::default();
    }

    let p_off = u32::from_be_bytes(header_bytes[84..88].try_into().unwrap_or([0; 4])) as u64;
    let p_len = u32::from_be_bytes(header_bytes[88..92].try_into().unwrap_or([0; 4])) as u64;
    let preview_loc = if p_off > 0 && p_len > 0 {
        Some(PreviewLocation {
            offset: p_off,
            length: p_len,
        })
    } else {
        None
    };

    let meta_offset = u32::from_be_bytes(header_bytes[92..96].try_into().unwrap_or([0; 4])) as u64;
    let meta_length =
        u32::from_be_bytes(header_bytes[96..100].try_into().unwrap_or([0; 4])) as usize;

    let mut sensor_dims = None;
    if meta_offset > 0 && meta_length > 0 {
        if let Some(end) = meta_offset.checked_add(meta_length as u64)
            && end as usize <= header_bytes.len()
        {
            sensor_dims =
                get_fuji_raf_dimensions(&header_bytes[meta_offset as usize..end as usize]);
        } else if let Some(src) = source
            && meta_length <= 64 * 1024
            && let Ok(meta_bytes) = read_source_range_up_to(src, meta_offset, meta_length)
        {
            sensor_dims = get_fuji_raf_dimensions(&meta_bytes);
        }
    }

    let mut orientation = image::metadata::Orientation::NoTransforms;
    let mut thumbnail = None;

    if let Some(ploc) = preview_loc {
        let p_slice_owned;
        let p_slice: Option<&[u8]> = if let Some(slice) = ploc.slice_in_bounds(header_bytes) {
            Some(slice)
        } else if (ploc.offset as usize) < header_bytes.len()
            && header_bytes.len() - (ploc.offset as usize) >= 4096
        {
            Some(&header_bytes[ploc.offset as usize..])
        } else if let Some(src) = source {
            p_slice_owned = read_source_range_up_to(src, ploc.offset, 16 * 1024).ok();
            p_slice_owned.as_deref()
        } else {
            None
        };

        if let Some(slice) = p_slice {
            orientation = get_exif_orientation(slice);
            if let Some(j_thumb) = extract_jpeg_thumbnail_location(slice) {
                thumbnail = Some(PreviewLocation {
                    offset: ploc.offset.saturating_add(j_thumb.offset),
                    length: j_thumb.length,
                });
            }
            if sensor_dims.is_none() {
                sensor_dims = read_jpeg_dimensions(slice);
            }
        }
    }

    let mut candidates_desc = Vec::new();
    if let Some(p) = preview_loc {
        candidates_desc.push(p);
    }
    if let Some(t) = thumbnail
        && !candidates_desc.contains(&t)
    {
        candidates_desc.push(t);
    }
    candidates_desc.sort_by(|a, b| {
        b.length
            .cmp(&a.length)
            .then_with(|| a.offset.cmp(&b.offset))
    });

    RawHeaderProbe {
        preview: preview_loc,
        medium: None,
        thumbnail,
        candidates_desc,
        sensor_dims,
        orientation,
    }
}

/// Unified header-probe result for Camera RAW and TIFF/EXIF containers.
pub fn probe_raw_header(header_bytes: &[u8], source: Option<&ImageSource>) -> RawHeaderProbe {
    if header_bytes.starts_with(b"FUJIFILM") {
        probe_fuji_raf_header(header_bytes, source)
    } else {
        probe_tiff_raw_header(header_bytes, source)
    }
}

/// Extracts embedded JPEG preview from RAW header bytes, reading remaining bytes via `read_source_range` if necessary.
pub fn read_raw_preview_bytes_with_header(
    source: &ImageSource,
    header_bytes: &[u8],
) -> Option<Vec<u8>> {
    let probe = probe_raw_header(header_bytes, Some(source));
    for loc in &probe.candidates_desc {
        if let Some(bytes) = read_location_bytes(header_bytes, Some(source), *loc)
            && is_decodable_jpeg(&bytes)
        {
            return Some(bytes);
        }
    }
    extract_jpeg_preview(header_bytes)
}

/// Helper to read raw preview JPEG bytes.
pub fn read_raw_preview_bytes(source: &ImageSource) -> Option<Vec<u8>> {
    let header_bytes = read_source_bytes_limited(source, RAW_EXIF_PROBE_LIMIT).ok()?;
    read_raw_preview_bytes_with_header(source, &header_bytes)
}

/// Helper to extract EXIF orientation from raw bytes.
pub fn get_exif_orientation(bytes: &[u8]) -> image::metadata::Orientation {
    let exif = match parse_tolerant_exif(bytes) {
        Some(exif) => exif,
        None => return image::metadata::Orientation::NoTransforms,
    };
    get_orientation_from_exif(&exif)
}

/// Tries to extract the smaller embedded JPEG thumbnail from EXIF APP1 metadata segment or TIFF/RAW container.
pub fn extract_jpeg_thumbnail(bytes: &[u8]) -> Option<Vec<u8>> {
    let probe = probe_raw_header(bytes, None);
    let mut order = Vec::new();
    if let Some(t) = probe.thumbnail {
        order.push(t);
    }
    if let Some(m) = probe.medium
        && !order.contains(&m)
    {
        order.push(m);
    }
    for c in probe.candidates_desc.iter().rev() {
        if !order.contains(c) {
            order.push(*c);
        }
    }
    for loc in order {
        if let Some(slice) = loc.slice_in_bounds(bytes)
            && is_decodable_jpeg(slice)
        {
            return Some(slice.to_vec());
        }
    }
    None
}

/// Tries to extract the larger/highest-resolution embedded JPEG preview from EXIF APP1 metadata segment or TIFF/RAW/RAF container.
pub fn extract_jpeg_preview(bytes: &[u8]) -> Option<Vec<u8>> {
    let probe = probe_raw_header(bytes, None);
    for loc in &probe.candidates_desc {
        if let Some(slice) = loc.slice_in_bounds(bytes)
            && is_decodable_jpeg(slice)
        {
            return Some(slice.to_vec());
        }
    }
    None
}

/// Helper to parse dimensions from standard EXIF metadata.
pub fn get_dimensions_from_exif(exif: &exif::Exif) -> Option<(u32, u32)> {
    let w = exif
        .get_field(exif::Tag::ImageWidth, exif::In::PRIMARY)
        .or_else(|| exif.get_field(exif::Tag::PixelXDimension, exif::In::PRIMARY))
        .and_then(|f| match f.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        })?;

    let h = exif
        .get_field(exif::Tag::ImageLength, exif::In::PRIMARY)
        .or_else(|| exif.get_field(exif::Tag::PixelYDimension, exif::In::PRIMARY))
        .and_then(|f| match f.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        })?;

    Some((w, h))
}
