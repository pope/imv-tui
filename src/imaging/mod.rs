//! Imaging module for loading, decoding, and resizing images.

pub mod types;
pub use types::*;

mod raw;
pub use raw::*;

mod decoder;
pub use decoder::*;

mod worker;
pub use worker::*;

use image::DynamicImage;
use ratatui_image::picker::Picker;
use std::path::PathBuf;
use std::sync::Arc;

/// The source input format for an image.
#[derive(Clone, Debug)]
pub enum ImageSource {
    /// A local filesystem image path.
    Local(PathBuf),
    /// A page inside a comic book zip archive (CBZ).
    Cbz {
        /// Path to the zip archive.
        zip_path: PathBuf,
        /// Name of the image entry inside the zip file.
        file_in_zip: String,
    },
}

impl ImageSource {
    /// Returns a unique absolute identifier string for the image source (local path or cbz path + page).
    pub fn identifier(&self) -> String {
        match self {
            Self::Local(path) => {
                let abs = if path.is_absolute() {
                    path.clone()
                } else if let Ok(curr) = std::env::current_dir() {
                    curr.join(path)
                } else {
                    path.clone()
                };
                abs.to_string_lossy().into_owned()
            }
            Self::Cbz {
                zip_path,
                file_in_zip,
            } => {
                let abs_zip = if zip_path.is_absolute() {
                    zip_path.clone()
                } else if let Ok(curr) = std::env::current_dir() {
                    curr.join(zip_path)
                } else {
                    zip_path.clone()
                };
                format!("{}::{}", abs_zip.to_string_lossy(), file_in_zip)
            }
        }
    }

    /// Generates a display name for listing the image source in the status bar/palettes.
    pub fn display_name(&self) -> String {
        match self {
            Self::Local(path) => path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("Unknown")
                .to_string(),
            Self::Cbz {
                zip_path,
                file_in_zip,
            } => {
                let zip_name = zip_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("Unknown");
                format!("{}: {}", zip_name, file_in_zip)
            }
        }
    }

    /// Returns true if the image source is a RAW format.
    pub fn is_raw(&self) -> bool {
        let path = match self {
            Self::Local(p) => p,
            Self::Cbz { zip_path, .. } => zip_path,
        };
        path.extension()
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
            .unwrap_or(false)
    }
}

/// A request payload sent to the resizing worker threads.
pub struct ResizeRequest {
    /// Shared reference to the decoded dynamic image.
    pub img: Arc<DynamicImage>,
    /// Expected full-resolution dimensions of the image.
    /// If the actual image buffer dimensions differ, it indicates a thumbnail placeholder.
    pub original_size: (u32, u32),
    /// Calculated scaling factor.
    pub scale: f64,
    /// Crop box geometry.
    pub crop: CropBox,
    /// Clamped image intersection region.
    pub intersection: ImageIntersection,
    /// Resized target width.
    pub target_w: u32,
    /// Resized target height.
    pub target_h: u32,
    /// Desired scaling filter.
    pub filter_type: FilterType,
    /// Protocol generator picker.
    pub picker: Picker,
    /// Brightness correction value.
    pub brightness: Brightness,
    /// Contrast correction percentage.
    pub contrast: Contrast,
    /// Output terminal grid cell dimensions.
    pub rendered_size_cells: (u16, u16),
    /// Sequence identifier to filter out stale requests.
    pub sequence: u64,
}

/// A request sent to the persistent loader thread.
pub struct LoaderRequest {
    /// File index inside the App source list.
    pub idx: usize,
    /// The ImageSource identifier.
    pub source: ImageSource,
    /// Whether this is a background prefetch request.
    pub is_prefetch: bool,
    /// Navigation sequence identifier to discard outdated loads.
    pub sequence: u64,
}

/// Encapsulates success values for a decoded image.
pub struct DecodedImage {
    /// The decoded DynamicImage.
    pub image: DynamicImage,
    /// Width of the image in pixels.
    pub width: u32,
    /// Height of the image in pixels.
    pub height: u32,
    /// Optional format parsed during load.
    pub format: Option<image::ImageFormat>,
    /// The size of the raw source file in bytes.
    pub disk_size: u64,
    /// The original raw/sensor width of the raw image, if any.
    pub raw_width: Option<u32>,
    /// The original raw/sensor height of the raw image, if any.
    pub raw_height: Option<u32>,
}

/// A response returned by the persistent loader thread.
pub struct LoaderResponse {
    /// File index inside the App source list.
    pub idx: usize,
    /// The decode result containing the DecodedImage data.
    pub result: Result<DecodedImage, String>,
    /// Whether this was a background prefetch request.
    pub is_prefetch: bool,
    /// Navigation sequence identifier of the load request.
    pub sequence: u64,
    /// Time taken to load and decode.
    pub decode_duration: std::time::Duration,
    /// Whether this response carries a fast-load low-res thumbnail image placeholder.
    pub is_thumbnail: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_is_image_file_logic() {
        let _ = fs::create_dir_all("target/tmp");
        let txt_path = PathBuf::from("target/tmp/fake_image.png");
        fs::write(&txt_path, "not an image").unwrap();

        // Without check_magic, it matches by extension .png
        assert!(is_image_file(&txt_path, false));

        // With check_magic, it validates bytes and rejects it
        assert!(!is_image_file(&txt_path, true));

        let _ = fs::remove_file(txt_path);
    }

    #[test]
    fn test_recursive_directory_scan() {
        let _ = fs::create_dir_all("target/tmp/scan_test/sub");
        let img1 = PathBuf::from("target/tmp/scan_test/img1.png");
        let img2 = PathBuf::from("target/tmp/scan_test/sub/img2.jpg");
        fs::write(&img1, "fake").unwrap();
        fs::write(&img2, "fake").unwrap();

        // Scan non-recursively
        let (non_rec_files, _) =
            scan_directory(&PathBuf::from("target/tmp/scan_test"), false, false).unwrap();
        assert_eq!(non_rec_files.len(), 1);
        assert_eq!(non_rec_files[0].file_name().unwrap(), "img1.png");

        // Scan recursively
        let (rec_files, _) =
            scan_directory(&PathBuf::from("target/tmp/scan_test"), false, true).unwrap();
        assert_eq!(rec_files.len(), 2);

        let _ = fs::remove_dir_all("target/tmp/scan_test");

        // Verify file path deduplication (e.g. symlinks pointing to the same file)
        #[cfg(unix)]
        {
            let _ = fs::create_dir_all("target/tmp/scan_test");
            let img1 = PathBuf::from("target/tmp/scan_test/img1.png");
            let sym = PathBuf::from("target/tmp/scan_test/symlink_img1.png");
            fs::write(&img1, "fake").unwrap();
            let _ = std::os::unix::fs::symlink(&img1, &sym);

            let (rec_files, _) =
                scan_directory(&PathBuf::from("target/tmp/scan_test"), false, true).unwrap();
            assert_eq!(rec_files.len(), 1);

            let _ = fs::remove_dir_all("target/tmp/scan_test");
        }
    }

    #[test]
    fn test_magic_bytes_guessing() {
        let dir = PathBuf::from("target/tmp/magic_test");
        let _ = fs::create_dir_all(&dir);

        let jpg_path = dir.join("test.jpg");
        fs::write(&jpg_path, [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46]).unwrap();

        let png_path = dir.join("test.png");
        fs::write(&png_path, [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]).unwrap();

        let zip_path = dir.join("test.zip");
        fs::write(&zip_path, [0x50, 0x4B, 0x03, 0x04, 0x0A, 0x00, 0x00, 0x00]).unwrap();

        let txt_path = dir.join("test.txt");
        fs::write(&txt_path, b"hello world this is a text file").unwrap();

        // Test guess_file_type
        assert_eq!(guess_file_type(&jpg_path), Some(GuessType::Image));
        assert_eq!(guess_file_type(&png_path), Some(GuessType::Image));
        assert_eq!(guess_file_type(&zip_path), Some(GuessType::Zip));
        assert_eq!(guess_file_type(&txt_path), None);

        // Test is_image_file with check_magic = true
        assert!(is_image_file(&jpg_path, true));
        assert!(is_image_file(&png_path, true));
        assert!(!is_image_file(&zip_path, true));
        assert!(!is_image_file(&txt_path, true));

        // Test is_cbz_or_zip with check_magic = true
        assert!(is_cbz_or_zip(&zip_path, true));
        assert!(!is_cbz_or_zip(&jpg_path, true));
        assert!(!is_cbz_or_zip(&txt_path, true));

        // Test with check_magic = false (extension-only)
        assert!(is_image_file(&jpg_path, false));
        assert!(is_cbz_or_zip(&zip_path, false));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_cbz_page_decoding() {
        let zip_path = PathBuf::from("target/tmp/test_cbz.cbz");
        let _ = fs::create_dir_all("target/tmp");

        {
            let file = fs::File::create(&zip_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::FileOptions::<()>::default();

            zip.start_file("cover.png", options).unwrap();
            use std::io::Write;
            zip.write_all(b"fake png data").unwrap();

            zip.start_file("page1.jpg", options).unwrap();
            zip.write_all(b"fake jpg data").unwrap();

            zip.start_file("README.txt", options).unwrap();
            zip.write_all(b"text file").unwrap();

            zip.start_file("subfolder/page2.webp", options).unwrap();
            zip.write_all(b"fake webp data").unwrap();

            zip.finish().unwrap();
        }

        // Test list_cbz_pages
        let pages = list_cbz_pages(&zip_path).unwrap();
        assert_eq!(pages.len(), 3);
        assert_eq!(pages[0], "cover.png");
        assert_eq!(pages[1], "page1.jpg");
        assert_eq!(pages[2], "subfolder/page2.webp");

        // Test read_source_bytes_limited from inside CBZ
        let src = ImageSource::Cbz {
            zip_path: zip_path.clone(),
            file_in_zip: "cover.png".to_string(),
        };
        let bytes_limited = read_source_bytes_limited(&src, 4).unwrap();
        assert_eq!(bytes_limited, b"fake");

        let bytes_all = read_source_bytes(&src).unwrap();
        assert_eq!(bytes_all, b"fake png data");

        // Test collect_sources
        let sources = collect_sources(std::slice::from_ref(&zip_path), true).unwrap();
        assert_eq!(sources.len(), 3);
        match &sources[0] {
            ImageSource::Cbz { file_in_zip, .. } => assert_eq!(file_in_zip, "cover.png"),
            _ => panic!("Expected Cbz source"),
        }

        let _ = fs::remove_file(zip_path);
    }

    #[test]
    fn test_read_source_bytes_limited_local() {
        let dir = PathBuf::from("target/tmp/limited_test");
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("test.dat");
        fs::write(&path, b"0123456789").unwrap();

        let src = ImageSource::Local(path.clone());
        let limited = read_source_bytes_limited(&src, 5).unwrap();
        assert_eq!(limited, b"01234");

        let unlimited = read_source_bytes_limited(&src, 50).unwrap();
        assert_eq!(unlimited, b"0123456789");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_empty_or_corrupt_exif() {
        let empty_bytes = &[];
        assert_eq!(
            get_exif_orientation(empty_bytes),
            image::metadata::Orientation::NoTransforms
        );
        assert!(extract_jpeg_thumbnail(empty_bytes).is_none());

        let corrupt_bytes = &[0x01, 0x02, 0x03, 0x04];
        assert_eq!(
            get_exif_orientation(corrupt_bytes),
            image::metadata::Orientation::NoTransforms
        );
        assert!(extract_jpeg_thumbnail(corrupt_bytes).is_none());
    }

    #[test]
    fn test_raw_image_file_recognition() {
        // Assert RAW extensions are recognized as valid image files
        let dir = PathBuf::from("target/tmp/raw_test");
        let _ = fs::create_dir_all(&dir);

        let dng_file = dir.join("test.dng");
        let nef_file = dir.join("test.nef");
        let cr2_file = dir.join("test.cr2");
        let arw_file = dir.join("test.arw");

        fs::write(&dng_file, []).unwrap();
        fs::write(&nef_file, []).unwrap();
        fs::write(&cr2_file, []).unwrap();
        fs::write(&arw_file, []).unwrap();

        assert!(is_image_file(&dng_file, false));
        assert!(is_image_file(&nef_file, false));
        assert!(is_image_file(&cr2_file, false));
        assert!(is_image_file(&arw_file, false));

        assert!(!is_cbz_or_zip(&dng_file, false));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_raf_preview_extraction() {
        let mut raf_bytes = vec![0u8; 100];
        // Write Fuji magic signature
        raf_bytes[0..8].copy_from_slice(b"FUJIFILM");

        // Write JPEG offset = 92
        let offset = 92u32.to_be_bytes();
        raf_bytes[84..88].copy_from_slice(&offset);

        // Write JPEG length = 4
        let length = 4u32.to_be_bytes();
        raf_bytes[88..92].copy_from_slice(&length);

        // Write mock JPEG bytes
        raf_bytes[92..96].copy_from_slice(b"JPEG");

        let extracted = extract_jpeg_thumbnail(&raf_bytes).unwrap();
        assert_eq!(extracted, b"JPEG");
    }

    #[test]
    fn test_arw_diagnostic() {
        let path =
            PathBuf::from("/home/pope/Pictures/2017-08-16.KayJay/Capture/20170816-KayJay-0121.ARW");
        if path.exists() {
            let bytes = std::fs::read(&path).unwrap();
            let partial = &bytes[..256 * 1024];
            println!("ARW file size: {}", bytes.len());

            let mut cursor = std::io::Cursor::new(partial);
            let exif = exif::Reader::new().read_from_container(&mut cursor);
            match exif {
                Ok(exif) => {
                    println!("EXIF successfully parsed!");
                    for f in exif.fields() {
                        if f.tag == exif::Tag::JPEGInterchangeFormat
                            || f.tag == exif::Tag::JPEGInterchangeFormatLength
                        {
                            println!(
                                "Tag: {:?}, IFD: {:?}, Value: {:?}",
                                f.tag, f.ifd_num, f.value
                            );
                        }
                    }
                    let thumb = extract_jpeg_thumbnail(partial);
                    println!("extracted thumbnail size: {:?}", thumb.map(|t| t.len()));

                    let thumb_and_dims = decode_thumbnail_and_dimensions(partial);
                    assert!(thumb_and_dims.is_some());
                    let (_, w, h) = thumb_and_dims.unwrap();
                    println!("Parsed RAW dimensions: {}x{}", w, h);
                    assert!(w > 0 && h > 0);
                }
                Err(e) => {
                    println!("EXIF parse error: {:?}", e);
                }
            }
        }
    }

    #[test]
    fn test_read_source_range_limits() {
        let path = PathBuf::from("target/tmp/nonexistent_file_limits.dat");
        let src = ImageSource::Local(path);
        // length > 64MB should fail with size limit error immediately
        let res = read_source_range(&src, 0, 65 * 1024 * 1024);
        assert!(res.is_err());
        assert!(res.unwrap_err().contains("too large"));

        // offset + length overflow should fail immediately in Cbz
        let cbz_src = ImageSource::Cbz {
            zip_path: PathBuf::from("target/tmp/nonexistent.zip"),
            file_in_zip: "test.png".to_string(),
        };
        let res = read_source_range(&cbz_src, u64::MAX - 10, 20);
        assert!(res.is_err());
    }

    #[test]
    fn test_cbz_read_source_range_streaming() {
        let zip_path = PathBuf::from("target/tmp/test_cbz_stream.cbz");
        let _ = fs::create_dir_all("target/tmp");

        {
            let file = fs::File::create(&zip_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::FileOptions::<()>::default();

            zip.start_file("data.bin", options).unwrap();
            use std::io::Write;
            zip.write_all(b"0123456789abcdef").unwrap();
            zip.finish().unwrap();
        }

        let src = ImageSource::Cbz {
            zip_path: zip_path.clone(),
            file_in_zip: "data.bin".to_string(),
        };

        // Read offset=4, length=4 (should return "4567")
        let res = read_source_range(&src, 4, 4).unwrap();
        assert_eq!(res, b"4567");

        // Read out of bounds should error gracefully
        let res = read_source_range(&src, 10, 10);
        assert!(res.is_err());

        let _ = fs::remove_file(zip_path);
    }

    #[test]
    fn test_fuji_raf_dimensions_parser() {
        let mut mock_data = [0u8; 200];
        mock_data[0..8].copy_from_slice(b"FUJIFILM");
        // Meta container offset = 100, length = 100
        mock_data[92..96].copy_from_slice(&100u32.to_be_bytes());
        mock_data[96..100].copy_from_slice(&100u32.to_be_bytes());

        // Inside Meta Container: offset 100
        // num_records = 1
        mock_data[100..104].copy_from_slice(&1u32.to_be_bytes());
        // Record 0: tag_id = 0x0111, tag_len = 4
        mock_data[104..106].copy_from_slice(&0x0111u16.to_be_bytes());
        mock_data[106..108].copy_from_slice(&4u16.to_be_bytes());
        // Data: height = 6192, width = 8256
        mock_data[108..110].copy_from_slice(&6192u16.to_be_bytes());
        mock_data[110..112].copy_from_slice(&8256u16.to_be_bytes());

        let res = get_fuji_raf_dimensions(&mock_data[100..]);
        assert_eq!(res, Some((8256, 6192)));
    }
}
