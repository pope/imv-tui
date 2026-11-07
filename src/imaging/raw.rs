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

/// Returns true if `bytes` starts with a TIFF container header (`II*\0` or `MM\0*`).
pub fn is_tiff_header(bytes: &[u8]) -> bool {
    bytes.len() >= 4
        && ((bytes[0] == 0x49 && bytes[1] == 0x49 && bytes[2] == 0x2A && bytes[3] == 0x00)
            || (bytes[0] == 0x4D && bytes[1] == 0x4D && bytes[2] == 0x00 && bytes[3] == 0x2A))
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

/// Extracts embedded JPEG preview from RAW header bytes, reading remaining bytes via `read_source_range` if necessary.
pub fn read_raw_preview_bytes_with_header(
    source: &ImageSource,
    header_bytes: &[u8],
) -> Option<Vec<u8>> {
    if header_bytes.starts_with(b"FUJIFILM") {
        if header_bytes.len() < 92 {
            return None;
        }
        let offset = u32::from_be_bytes(header_bytes[84..88].try_into().ok()?) as u64;
        let length = u32::from_be_bytes(header_bytes[88..92].try_into().ok()?) as u64;

        if let Some(end) = offset.checked_add(length)
            && end as usize <= header_bytes.len()
        {
            let candidate = &header_bytes[offset as usize..end as usize];
            if is_decodable_jpeg(candidate) {
                return Some(candidate.to_vec());
            }
        }
        let buf = read_source_range(source, offset, length).ok()?;
        if is_decodable_jpeg(&buf) {
            return Some(buf);
        }
        return None;
    }

    let exif = parse_tolerant_exif(header_bytes)?;
    let mut candidates = collect_exif_jpeg_candidates(&exif);
    // Sort by length descending (largest first)
    candidates.sort_by_key(|&(_, len)| std::cmp::Reverse(len));

    let is_tiff = is_tiff_header(header_bytes);
    let tiff_start = if is_tiff {
        0
    } else {
        header_bytes
            .windows(6)
            .position(|window| window == b"Exif\0\0")? as u64
            + 6
    };

    for (offset, length) in candidates {
        let Some(start) = tiff_start.checked_add(offset) else {
            continue;
        };

        // In-memory fast path: if the candidate is fully within header_bytes
        if let Some(end) = start.checked_add(length)
            && (end as usize) <= header_bytes.len()
        {
            let candidate = &header_bytes[start as usize..end as usize];
            if is_decodable_jpeg(candidate) {
                return Some(candidate.to_vec());
            }
            continue;
        }

        // If start is within header_bytes and doesn't even look like a decodable JPEG header, skip reading from disk!
        if (start as usize) < header_bytes.len()
            && !is_decodable_jpeg(&header_bytes[start as usize..])
        {
            continue;
        }

        // Otherwise, read from source
        if let Ok(buf) = read_source_range(source, start, length)
            && is_decodable_jpeg(&buf)
        {
            return Some(buf);
        }
    }

    None
}

/// Helper to read raw preview JPEG bytes.
pub fn read_raw_preview_bytes(source: &ImageSource) -> Option<Vec<u8>> {
    let header_bytes = read_source_bytes_limited(source, RAW_EXIF_PROBE_LIMIT).ok()?;
    read_raw_preview_bytes_with_header(source, &header_bytes)
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
    // 1. Detect Fujifilm RAF raw formats (which are not standard TIFF files)
    if bytes.starts_with(b"FUJIFILM") {
        if bytes.len() < 92 {
            return None;
        }
        let offset = u32::from_be_bytes(bytes[84..88].try_into().ok()?) as usize;
        let length = u32::from_be_bytes(bytes[88..92].try_into().ok()?) as usize;
        let end = offset.checked_add(length)?;
        if end <= bytes.len() {
            let candidate = &bytes[offset..end];
            if is_decodable_jpeg(candidate) {
                return Some(candidate.to_vec());
            }
        }
        return None;
    }

    // 2. Parse standard TIFF/EXIF structures for other formats
    let exif = parse_tolerant_exif(bytes)?;
    let mut candidates = collect_exif_jpeg_candidates(&exif);
    // Sort by length ascending (smallest first)
    candidates.sort_by_key(|&(_, len)| len);

    let is_tiff = is_tiff_header(bytes);
    let tiff_start = if is_tiff {
        0
    } else {
        (bytes.windows(6).position(|window| window == b"Exif\0\0")? + 6) as u64
    };

    // Pick the first candidate within bytes where is_decodable_jpeg is true
    for (offset, length) in candidates {
        if let Some(start) = tiff_start.checked_add(offset)
            && let Some(end) = start.checked_add(length)
            && (end as usize) <= bytes.len()
        {
            let candidate = &bytes[start as usize..end as usize];
            if is_decodable_jpeg(candidate) {
                return Some(candidate.to_vec());
            }
        }
    }

    None
}

/// Tries to extract the larger/highest-resolution embedded JPEG preview from EXIF APP1 metadata segment or TIFF/RAW/RAF container.
pub fn extract_jpeg_preview(bytes: &[u8]) -> Option<Vec<u8>> {
    // 1. Detect Fujifilm RAF raw formats (which are not standard TIFF files)
    if bytes.starts_with(b"FUJIFILM") {
        if bytes.len() < 92 {
            return None;
        }
        let offset = u32::from_be_bytes(bytes[84..88].try_into().ok()?) as usize;
        let length = u32::from_be_bytes(bytes[88..92].try_into().ok()?) as usize;
        let end = offset.checked_add(length)?;
        if end <= bytes.len() {
            let candidate = &bytes[offset..end];
            if is_decodable_jpeg(candidate) {
                return Some(candidate.to_vec());
            }
        }
        return None;
    }

    // 2. Parse standard TIFF/EXIF structures for other formats
    let exif = parse_tolerant_exif(bytes)?;
    let mut candidates = collect_exif_jpeg_candidates(&exif);
    // Sort by length descending (largest first)
    candidates.sort_by_key(|&(_, len)| std::cmp::Reverse(len));

    let is_tiff = is_tiff_header(bytes);
    let tiff_start = if is_tiff {
        0
    } else {
        (bytes.windows(6).position(|window| window == b"Exif\0\0")? + 6) as u64
    };

    // Pick the first candidate within bytes where is_decodable_jpeg is true
    for (offset, length) in candidates {
        if let Some(start) = tiff_start.checked_add(offset)
            && let Some(end) = start.checked_add(length)
            && (end as usize) <= bytes.len()
        {
            let candidate = &bytes[start as usize..end as usize];
            if is_decodable_jpeg(candidate) {
                return Some(candidate.to_vec());
            }
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
