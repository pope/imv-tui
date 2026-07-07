//! Low-level image file range loading, RAW metadata parsing, and EXIF processing.

use std::io::{Read, Seek};

use crate::imaging::ImageSource;

/// Reads the source raw bytes from local files or CBZ zip archives.
pub fn read_source_bytes(source: &ImageSource) -> Result<Vec<u8>, String> {
    let is_raw_extension = match source {
        ImageSource::Local(path) => path.extension(),
        ImageSource::Cbz { zip_path, .. } => zip_path.extension(),
    }
    .and_then(|ext| ext.to_str())
    .map(|ext| {
        ext.eq_ignore_ascii_case("dng")
            || ext.eq_ignore_ascii_case("cr2")
            || ext.eq_ignore_ascii_case("nef")
            || ext.eq_ignore_ascii_case("arw")
            || ext.eq_ignore_ascii_case("orf")
            || ext.eq_ignore_ascii_case("rw2")
            || ext.eq_ignore_ascii_case("pef")
            || ext.eq_ignore_ascii_case("raf")
    })
    .unwrap_or(false);

    if is_raw_extension && let Some(preview_bytes) = read_raw_preview_bytes(source) {
        return Ok(preview_bytes);
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

/// Helper to read raw preview JPEG bytes.
pub fn read_raw_preview_bytes(source: &ImageSource) -> Option<Vec<u8>> {
    let header_bytes = read_source_bytes_limited(source, 256 * 1024).ok()?;

    if header_bytes.starts_with(b"FUJIFILM") {
        if header_bytes.len() < 92 {
            return None;
        }
        let offset = u32::from_be_bytes(header_bytes[84..88].try_into().ok()?) as u64;
        let length = u32::from_be_bytes(header_bytes[88..92].try_into().ok()?) as u64;
        return read_source_range(source, offset, length).ok();
    }

    let mut cursor = std::io::Cursor::new(&header_bytes);
    let exif = exif::Reader::new().read_from_container(&mut cursor).ok()?;

    let primary_offset = exif.get_field(exif::Tag::JPEGInterchangeFormat, exif::In::PRIMARY);
    let primary_length = exif.get_field(exif::Tag::JPEGInterchangeFormatLength, exif::In::PRIMARY);

    let thumb_offset = exif.get_field(exif::Tag::JPEGInterchangeFormat, exif::In::THUMBNAIL);
    let thumb_length = exif.get_field(exif::Tag::JPEGInterchangeFormatLength, exif::In::THUMBNAIL);

    let mut best_offset = None;
    let mut best_length = 0;

    if let Some(f_off) = primary_offset
        && let Some(f_len) = primary_length
    {
        let off = match f_off.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        };
        let len = match f_len.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        };
        if let Some(off) = off
            && let Some(len) = len
        {
            best_offset = Some(off as u64);
            best_length = len as u64;
        }
    }

    if let Some(f_off) = thumb_offset
        && let Some(f_len) = thumb_length
    {
        let off = match f_off.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        };
        let len = match f_len.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        };
        if let Some(off) = off
            && let Some(len) = len
            && len as u64 > best_length
        {
            best_offset = Some(off as u64);
            best_length = len as u64;
        }
    }

    let offset = best_offset?;
    let length = best_length;

    let is_tiff = header_bytes.len() >= 4
        && ((header_bytes[0] == 0x49
            && header_bytes[1] == 0x49
            && header_bytes[2] == 0x2A
            && header_bytes[3] == 0x00)
            || (header_bytes[0] == 0x4D
                && header_bytes[1] == 0x4D
                && header_bytes[2] == 0x00
                && header_bytes[3] == 0x2A));

    let tiff_start = if is_tiff {
        0
    } else {
        header_bytes
            .windows(6)
            .position(|window| window == b"Exif\0\0")? as u64
            + 6
    };

    let start = tiff_start.checked_add(offset)?;
    read_source_range(source, start, length).ok()
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
    let mut cursor = std::io::Cursor::new(bytes);
    let exif = match exif::Reader::new().read_from_container(&mut cursor) {
        Ok(exif) => exif,
        Err(_) => return image::metadata::Orientation::NoTransforms,
    };
    let val = exif
        .get_field(exif::Tag::Orientation, exif::In::PRIMARY)
        .and_then(|f| match f.value {
            exif::Value::Short(ref v) => v.first().copied(),
            _ => None,
        })
        .unwrap_or(1);
    match val {
        1 => image::metadata::Orientation::NoTransforms,
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
            return Some(bytes[offset..end].to_vec());
        }
        return None;
    }

    // 2. Parse standard TIFF/EXIF structures for other formats
    let mut cursor = std::io::Cursor::new(bytes);
    let exif = exif::Reader::new().read_from_container(&mut cursor).ok()?;

    let primary_offset = exif.get_field(exif::Tag::JPEGInterchangeFormat, exif::In::PRIMARY);
    let primary_length = exif.get_field(exif::Tag::JPEGInterchangeFormatLength, exif::In::PRIMARY);

    let thumb_offset = exif.get_field(exif::Tag::JPEGInterchangeFormat, exif::In::THUMBNAIL);
    let thumb_length = exif.get_field(exif::Tag::JPEGInterchangeFormatLength, exif::In::THUMBNAIL);

    let mut candidates = Vec::new();

    if let Some(f_off) = primary_offset
        && let Some(f_len) = primary_length
    {
        let off = match f_off.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        };
        let len = match f_len.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        };
        if let Some(off) = off
            && let Some(len) = len
        {
            candidates.push((off as usize, len as usize));
        }
    }

    if let Some(f_off) = thumb_offset
        && let Some(f_len) = thumb_length
    {
        let off = match f_off.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        };
        let len = match f_len.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        };
        if let Some(off) = off
            && let Some(len) = len
        {
            candidates.push((off as usize, len as usize));
        }
    }

    // Sort by length ascending (smallest first)
    candidates.sort_by_key(|&(_, len)| len);

    // TIFF magic headers: II*\0 or MM\0*
    let is_tiff = bytes.len() >= 4
        && ((bytes[0] == 0x49 && bytes[1] == 0x49 && bytes[2] == 0x2A && bytes[3] == 0x00)
            || (bytes[0] == 0x4D && bytes[1] == 0x4D && bytes[2] == 0x00 && bytes[3] == 0x2A));

    let tiff_start = if is_tiff {
        0
    } else {
        bytes.windows(6).position(|window| window == b"Exif\0\0")? + 6
    };

    // Pick the smallest candidate that fits within the provided bytes slice
    for (offset, length) in candidates {
        if let Some(start) = tiff_start.checked_add(offset)
            && let Some(end) = start.checked_add(length)
            && end <= bytes.len()
        {
            return Some(bytes[start..end].to_vec());
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
            return Some(bytes[offset..end].to_vec());
        }
        return None;
    }

    // 2. Parse standard TIFF/EXIF structures for other formats
    let mut cursor = std::io::Cursor::new(bytes);
    let exif = exif::Reader::new().read_from_container(&mut cursor).ok()?;

    let primary_offset = exif.get_field(exif::Tag::JPEGInterchangeFormat, exif::In::PRIMARY);
    let primary_length = exif.get_field(exif::Tag::JPEGInterchangeFormatLength, exif::In::PRIMARY);

    let thumb_offset = exif.get_field(exif::Tag::JPEGInterchangeFormat, exif::In::THUMBNAIL);
    let thumb_length = exif.get_field(exif::Tag::JPEGInterchangeFormatLength, exif::In::THUMBNAIL);

    let mut best_offset = None;
    let mut best_length = 0;

    if let Some(f_off) = primary_offset
        && let Some(f_len) = primary_length
    {
        let off = match f_off.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        };
        let len = match f_len.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        };
        if let Some(off) = off
            && let Some(len) = len
        {
            best_offset = Some(off as usize);
            best_length = len as usize;
        }
    }

    if let Some(f_off) = thumb_offset
        && let Some(f_len) = thumb_length
    {
        let off = match f_off.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        };
        let len = match f_len.value {
            exif::Value::Long(ref v) => v.first().copied(),
            exif::Value::Short(ref v) => v.first().map(|&x| x as u32),
            _ => None,
        };
        if let Some(off) = off
            && let Some(len) = len
            && len as usize > best_length
        {
            best_offset = Some(off as usize);
            best_length = len as usize;
        }
    }

    let offset = best_offset?;
    let length = best_length;

    // TIFF magic headers: II*\0 or MM\0*
    let is_tiff = bytes.len() >= 4
        && ((bytes[0] == 0x49 && bytes[1] == 0x49 && bytes[2] == 0x2A && bytes[3] == 0x00)
            || (bytes[0] == 0x4D && bytes[1] == 0x4D && bytes[2] == 0x00 && bytes[3] == 0x2A));

    let tiff_start = if is_tiff {
        0
    } else {
        bytes.windows(6).position(|window| window == b"Exif\0\0")? + 6
    };

    let start = tiff_start.checked_add(offset)?;
    let end = start.checked_add(length)?;

    if end <= bytes.len() {
        Some(bytes[start..end].to_vec())
    } else {
        None
    }
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
