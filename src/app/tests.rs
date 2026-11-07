use super::*;
use crate::app::cache::CachedImage;
use crate::app::classifications::ClassificationJsonItem;
use crate::config::InfoBarPosition;
use crate::imaging::types::{Brightness, Contrast, Rotation};
use crate::imaging::{ImageSource, ScaleMode};
use image::DynamicImage;
use ratatui_image::picker::{Picker, ProtocolType};
use std::fs;
use std::path::PathBuf;

#[test]
fn test_import_export_text_and_json() {
    let _ = std::fs::create_dir_all("target/tmp");
    let json_path = PathBuf::from("target/tmp/test_classifications.json");
    let txt_path = PathBuf::from("target/tmp/test_classifications.txt");

    let images = vec![
        ImageSource::Local(PathBuf::from("img1.png")),
        ImageSource::Local(PathBuf::from("img2.png")),
        ImageSource::Cbz {
            zip_path: PathBuf::from("archive.cbz"),
            file_in_zip: "page1.png".to_string(),
        },
    ];

    let picker = Picker::halfblocks();
    let app_config = AppConfig {
        filter_type: crate::imaging::FilterType::Nearest,
        scale_mode: crate::imaging::ScaleMode::Shrink,
        disable_thumbnail: true,
        infobar: InfoBarPosition::Bottom,
        active_protocol: crate::config::ActiveProtocol::Halfblocks,
    };
    let scan_config = ScanConfig {
        initial_scan_path: PathBuf::from("."),
        check_magic: false,
        recursive: false,
        is_cbz: false,
        is_piped: false,
    };
    let (mut app, _channels) =
        App::new_mocked(images.clone(), 0, picker, app_config, scan_config).unwrap();

    app.queue.set_classification(0, Classification::Pick);
    app.queue.set_classification(1, Classification::Reject);
    app.queue.set_classification(2, Classification::Pick);

    app.adjustments_mut()[0] = ImageAdjustments {
        brightness: Brightness::new(20),
        contrast: Contrast::new(15.0),
        rotation: Rotation::D90,
    };
    app.adjustments_mut()[2] = ImageAdjustments {
        brightness: Brightness::new(-30),
        contrast: Contrast::ZERO,
        rotation: Rotation::D180,
    };

    // Test Export to JSON
    app.export_classifications(&json_path).unwrap();
    assert!(json_path.exists());
    let json_content = std::fs::read_to_string(&json_path).unwrap();
    // Check that it's serialized as a JSON array of objects
    let parsed_json: Vec<ClassificationJsonItem> = serde_json::from_str(&json_content).unwrap();
    assert_eq!(parsed_json.len(), 3);
    assert_eq!(parsed_json[0].flag, "picked");
    assert_eq!(parsed_json[0].archive, None);
    assert_eq!(parsed_json[0].brightness, Brightness::new(20));
    assert_eq!(parsed_json[0].contrast, Contrast::new(15.0));
    assert_eq!(parsed_json[0].rotation, Rotation::D90);
    assert_eq!(parsed_json[1].flag, "rejected");
    assert_eq!(parsed_json[1].archive, None);
    assert_eq!(parsed_json[1].brightness, Brightness::ZERO);
    assert_eq!(parsed_json[1].contrast, Contrast::ZERO);
    assert_eq!(parsed_json[1].rotation, Rotation::D0);
    assert_eq!(parsed_json[2].flag, "picked");
    assert!(parsed_json[2].archive.is_some());
    assert_eq!(parsed_json[2].filename, "page1.png");
    assert_eq!(parsed_json[2].brightness, Brightness::new(-30));
    assert_eq!(parsed_json[2].contrast, Contrast::ZERO);
    assert_eq!(parsed_json[2].rotation, Rotation::D180);

    // Test Export to Text
    app.export_classifications(&txt_path).unwrap();
    assert!(txt_path.exists());
    let txt_content = std::fs::read_to_string(&txt_path).unwrap();
    // Verify that the tab character is used to separate
    assert!(txt_content.contains("PICK\t"));
    assert!(txt_content.contains("REJECT\t"));
    // Text format does not contain adjustment parameters
    assert!(!txt_content.contains("20"));
    assert!(!txt_content.contains("15"));

    // Clear classifications and adjustments in app
    app.queue.clear_classifications_and_adjustments();

    // Import from JSON
    app.import_classifications(&json_path).unwrap();
    assert_eq!(app.classifications()[0], Classification::Pick);
    assert_eq!(app.classifications()[1], Classification::Reject);
    assert_eq!(app.classifications()[2], Classification::Pick);
    assert_eq!(app.adjustments()[0].brightness, Brightness::new(20));
    assert_eq!(app.adjustments()[0].contrast, Contrast::new(15.0));
    assert_eq!(app.adjustments()[0].rotation, Rotation::D90);
    assert_eq!(app.adjustments()[2].brightness, Brightness::new(-30));
    assert_eq!(app.adjustments()[2].contrast, Contrast::ZERO);
    assert_eq!(app.adjustments()[2].rotation, Rotation::D180);

    // Clear classifications and adjustments again
    app.queue.clear_classifications_and_adjustments();

    // Import from Text
    app.import_classifications(&txt_path).unwrap();
    assert_eq!(app.classifications()[0], Classification::Pick);
    assert_eq!(app.classifications()[1], Classification::Reject);
    assert_eq!(app.classifications()[2], Classification::Pick);
    assert_eq!(app.adjustments()[0], ImageAdjustments::default());

    // Clean up
    let _ = std::fs::remove_file(json_path);
    let _ = std::fs::remove_file(txt_path);
}

#[test]
fn test_unmapped_classifications_preservation() {
    let _ = std::fs::create_dir_all("target/tmp");
    let manifest_path = PathBuf::from("target/tmp/unmapped_test.json");

    let current_dir = std::env::current_dir().unwrap();
    let active_abs = current_dir
        .join("active.png")
        .to_string_lossy()
        .into_owned();
    let missing_abs = current_dir
        .join("missing.png")
        .to_string_lossy()
        .into_owned();

    // Write a mock manifest containing two files:
    // - active_abs (which will be present in the session queue)
    // - missing_abs (which will NOT be in the session queue)
    let mock_items = vec![
        ClassificationJsonItem {
            archive: None,
            filename: active_abs.clone(),
            flag: "picked".to_string(),
            brightness: Brightness::new(10),
            contrast: Contrast::ZERO,
            rotation: Rotation::D0,
        },
        ClassificationJsonItem {
            archive: None,
            filename: missing_abs.clone(),
            flag: "rejected".to_string(),
            brightness: Brightness::ZERO,
            contrast: Contrast::new(2.5),
            rotation: Rotation::D0,
        },
    ];
    let manifest_content = serde_json::to_string(&mock_items).unwrap();
    std::fs::write(&manifest_path, manifest_content).unwrap();

    let images = vec![ImageSource::Local(PathBuf::from("active.png"))];

    let picker = Picker::halfblocks();
    let app_config = AppConfig {
        filter_type: crate::imaging::FilterType::Nearest,
        scale_mode: crate::imaging::ScaleMode::Shrink,
        disable_thumbnail: true,
        infobar: InfoBarPosition::Bottom,
        active_protocol: crate::config::ActiveProtocol::Halfblocks,
    };
    let scan_config = ScanConfig {
        initial_scan_path: PathBuf::from("."),
        check_magic: false,
        recursive: false,
        is_cbz: false,
        is_piped: false,
    };
    let (mut app, _channels) = App::new_mocked(images, 0, picker, app_config, scan_config).unwrap();

    // Initially no unmapped files
    assert!(app.unmapped_classifications.is_empty());

    // Import the classifications
    app.import_classifications(&manifest_path).unwrap();

    // "active.png" should be mapped to the active classifications/adjustments
    assert_eq!(app.classifications()[0], Classification::Pick);
    assert_eq!(app.adjustments()[0].brightness, Brightness::new(10));

    // "missing.png" should be stored in the unmapped classifications
    assert_eq!(app.unmapped_classifications.len(), 1);
    let unmapped_val = app.unmapped_classifications.get(&active_abs);
    assert!(unmapped_val.is_none());
    let unmapped_val = app.unmapped_classifications.get(&missing_abs).unwrap();
    assert_eq!(unmapped_val.0, Classification::Reject);
    assert_eq!(unmapped_val.1.contrast, Contrast::new(2.5));

    // Now export classifications to a new JSON file
    let export_path = PathBuf::from("target/tmp/unmapped_test_exported.json");
    app.export_classifications(&export_path).unwrap();

    // Read the exported file and verify it contains BOTH files
    let exported_content = std::fs::read_to_string(&export_path).unwrap();
    let parsed_json: Vec<ClassificationJsonItem> = serde_json::from_str(&exported_content).unwrap();

    assert_eq!(parsed_json.len(), 2);

    // Find each item
    let active_item = parsed_json
        .iter()
        .find(|item| item.filename == active_abs)
        .unwrap();
    assert_eq!(active_item.flag, "picked");
    assert_eq!(active_item.brightness, Brightness::new(10));

    let missing_item = parsed_json
        .iter()
        .find(|item| item.filename == missing_abs)
        .unwrap();
    assert_eq!(missing_item.flag, "rejected");
    assert_eq!(missing_item.contrast, Contrast::new(2.5));

    // Clean up
    let _ = std::fs::remove_file(manifest_path);
    let _ = std::fs::remove_file(export_path);
}

#[test]
fn test_refresh_files_preservation() {
    let _ = fs::create_dir_all("target/tmp/refresh_test");
    let img1_path = PathBuf::from("target/tmp/refresh_test/img1.png");
    fs::write(&img1_path, "fake").unwrap();

    let images = vec![ImageSource::Local(img1_path.clone())];
    let picker = Picker::halfblocks();

    let app_config = AppConfig {
        filter_type: crate::imaging::FilterType::Nearest,
        scale_mode: crate::imaging::ScaleMode::Shrink,
        disable_thumbnail: true,
        infobar: InfoBarPosition::Bottom,
        active_protocol: crate::config::ActiveProtocol::Halfblocks,
    };
    let scan_config = ScanConfig {
        initial_scan_path: PathBuf::from("target/tmp/refresh_test"),
        check_magic: false,
        recursive: false,
        is_cbz: false,
        is_piped: false,
    };
    let (mut app, _channels) = App::new_mocked(images, 0, picker, app_config, scan_config).unwrap();

    // Flag the first image as a Pick
    app.queue.set_classification(0, Classification::Pick);
    app.adjustments_mut()[0].brightness = Brightness::new(15);

    // Add a second image to the directory
    let img2_path = PathBuf::from("target/tmp/refresh_test/img2.png");
    fs::write(&img2_path, "fake").unwrap();

    // Refresh the files
    app.refresh_files().unwrap();

    // Verify we now have 2 images
    assert_eq!(app.queue.len(), 2);

    let current_dir = std::env::current_dir().unwrap();
    let img1_abs = current_dir.join(&img1_path).to_string_lossy().into_owned();
    let img2_abs = current_dir.join(&img2_path).to_string_lossy().into_owned();

    // Verify the classification and adjustment for img1 are preserved
    // Find index of img1 in the new queue
    let img1_idx = app
        .queue
        .images()
        .iter()
        .position(|img| img.identifier() == img1_abs)
        .unwrap();
    assert_eq!(app.classifications()[img1_idx], Classification::Pick);
    assert_eq!(app.adjustments()[img1_idx].brightness, Brightness::new(15));

    // Verify img2 is unflagged
    let img2_idx = app
        .queue
        .images()
        .iter()
        .position(|img| img.identifier() == img2_abs)
        .unwrap();
    assert_eq!(app.classifications()[img2_idx], Classification::Unflagged);

    // Clean up
    let _ = fs::remove_dir_all("target/tmp/refresh_test");
}

#[test]
fn test_refresh_files_unmapped_migration() {
    let _ = fs::create_dir_all("target/tmp/refresh_unmapped_test");
    let img1_path = PathBuf::from("target/tmp/refresh_unmapped_test/img1.png");
    let img2_path = PathBuf::from("target/tmp/refresh_unmapped_test/img2.png");
    fs::write(&img1_path, "fake1").unwrap();
    fs::write(&img2_path, "fake2").unwrap();

    let images = vec![
        ImageSource::Local(img1_path.clone()),
        ImageSource::Local(img2_path.clone()),
    ];
    let picker = Picker::halfblocks();
    let app_config = AppConfig {
        filter_type: crate::imaging::FilterType::Nearest,
        scale_mode: crate::imaging::ScaleMode::Shrink,
        disable_thumbnail: true,
        infobar: InfoBarPosition::Bottom,
        active_protocol: crate::config::ActiveProtocol::Halfblocks,
    };
    let scan_config = ScanConfig {
        initial_scan_path: PathBuf::from("target/tmp/refresh_unmapped_test"),
        check_magic: false,
        recursive: false,
        is_cbz: false,
        is_piped: false,
    };
    let (mut app, _channels) = App::new_mocked(images, 0, picker, app_config, scan_config).unwrap();

    // Flag img2 and set contrast
    app.queue.set_classification(1, Classification::Reject);
    app.adjustments_mut()[1].contrast = Contrast::new(3.0);

    let current_dir = std::env::current_dir().unwrap();
    let img2_abs = current_dir.join(&img2_path).to_string_lossy().into_owned();

    // Delete img2 from disk
    fs::remove_file(&img2_path).unwrap();

    // Refresh files - img2 is now missing
    app.refresh_files().unwrap();

    // Verify we only have 1 active image (img1)
    assert_eq!(app.queue.len(), 1);

    // Verify img2's classification & adjustments migrated to unmapped_classifications
    let entry = app.unmapped_classifications.get(&img2_abs).unwrap();
    assert_eq!(entry.0, Classification::Reject);
    assert_eq!(entry.1.contrast, Contrast::new(3.0));

    // Re-create img2 on disk
    fs::write(&img2_path, "fake2").unwrap();

    // Refresh files again - img2 should be restored and removed from unmapped
    app.refresh_files().unwrap();
    assert_eq!(app.queue.len(), 2);

    let img2_idx = app
        .queue
        .images()
        .iter()
        .position(|img| img.identifier() == img2_abs)
        .unwrap();
    assert_eq!(app.classifications()[img2_idx], Classification::Reject);
    assert_eq!(app.adjustments()[img2_idx].contrast, Contrast::new(3.0));
    assert!(!app.unmapped_classifications.contains_key(&img2_abs));

    // Clean up
    let _ = fs::remove_dir_all("target/tmp/refresh_unmapped_test");
}

#[test]
fn test_prefetch_cache_retention_and_hits() {
    let images = vec![
        ImageSource::Local(PathBuf::from("img1.png")),
        ImageSource::Local(PathBuf::from("img2.png")),
    ];
    let picker = Picker::halfblocks();
    let app_config = AppConfig {
        filter_type: crate::imaging::FilterType::Nearest,
        scale_mode: crate::imaging::ScaleMode::Shrink,
        disable_thumbnail: true,
        infobar: InfoBarPosition::Bottom,
        active_protocol: crate::config::ActiveProtocol::Halfblocks,
    };
    let scan_config = ScanConfig {
        initial_scan_path: PathBuf::from("."),
        check_magic: false,
        recursive: false,
        is_cbz: false,
        is_piped: false,
    };
    let (mut app, _channels) = App::new_mocked(images, 0, picker, app_config, scan_config).unwrap();

    // Simulate background loading inserting into prefetch cache
    let shared_img = Arc::new(image::DynamicImage::ImageRgb8(image::RgbImage::new(10, 10)));
    {
        let mut cache = app.prefetch_cache.lock().unwrap();
        cache.insert(
            1,
            CachedImage {
                image: Some(shared_img.clone()),
                thumbnail: None,
                thumbnail_source: None,
                width: 10,
                height: 10,
                format: Some(image::ImageFormat::Png),
                decode_duration: std::time::Duration::from_millis(5),
                thumbnail_decode_duration: std::time::Duration::ZERO,
                disk_size: 100,
                raw_width: None,
                raw_height: None,
            },
        );
    }

    // Navigate to index 1
    app.execute_command(Command::NextImage);

    // Check that it was a cache hit!
    assert!(app.stats.is_prefetch_cache_hit);
    assert_eq!(app.stats.disk_size, 100);

    // Verify that it was NOT evicted from the cache on hit
    {
        let cache = app.prefetch_cache.lock().unwrap();
        assert!(cache.contains_key(&1));
    }

    // Verify sliding window indices helper
    assert_eq!(app.get_sliding_window_indices(), vec![0, 1]);
}

#[test]
fn test_sequence_number_protection() {
    let (mut app, channels) = setup_test_app_with_channels();
    while channels.loader_rx.try_recv().is_ok() {}

    // Bump current sequence
    app.current_sequence = 10;

    let dummy_img = image::DynamicImage::ImageRgb8(image::RgbImage::new(1, 1));
    let resp = LoaderResponse {
        idx: 0,
        is_prefetch: false,
        is_thumbnail: false,
        thumbnail_source: None,
        sequence: 9,
        result: Ok(DecodedImage {
            image: dummy_img,
            width: 1,
            height: 1,
            format: None,
            disk_size: 0,
            raw_width: None,
            raw_height: None,
        }),
        decode_duration: std::time::Duration::ZERO,
    };
    channels.response_tx.send(resp).unwrap();

    // Process channels
    let updated = app.update_channels();
    // Since sequence was 9 < 10, it should have skipped it
    assert!(updated); // received was true
    assert!(app.original_image.is_none());
}

#[test]
fn test_error_propagation_boundary() {
    let (mut app, channels) = setup_test_app_with_channels();
    while channels.loader_rx.try_recv().is_ok() {}

    // 1. Prefetch error at index 1 (should not update current image error state)
    let resp_prefetch_err = LoaderResponse {
        idx: 1,
        is_prefetch: true,
        is_thumbnail: false,
        thumbnail_source: None,
        sequence: app.current_sequence,
        result: Err("Failed to load prefetch".to_string()),
        decode_duration: std::time::Duration::ZERO,
    };
    channels.response_tx.send(resp_prefetch_err).unwrap();
    app.update_channels();
    assert!(app.error_message.is_none());

    // 2. Active load error at index 0 (current index)
    let resp_active_err = LoaderResponse {
        idx: 0,
        is_prefetch: false,
        is_thumbnail: false,
        thumbnail_source: None,
        sequence: app.current_sequence,
        result: Err("Corrupt image file".to_string()),
        decode_duration: std::time::Duration::ZERO,
    };
    channels.response_tx.send(resp_active_err).unwrap();
    app.update_channels();
    assert_eq!(app.error_message.as_deref(), Some("Corrupt image file"));
    assert!(app.original_image.is_none());
}

#[test]
fn test_start_load_image_channel_dispatch() {
    let (mut app, channels) = setup_test_app_with_channels();

    // On App::new_mocked, start_load_image() was called.
    // It dispatches the active load request for index 0 with sequence 1.
    let active_req = channels
        .loader_rx
        .try_recv()
        .expect("Expected active LoaderRequest on loader_rx");
    assert_eq!(active_req.idx, 0);
    assert!(!active_req.is_prefetch);
    assert_eq!(active_req.sequence, 1);
    assert_eq!(
        active_req.source,
        ImageSource::Local(PathBuf::from("img1.png"))
    );

    // It also dispatches prefetch request for index 1 with sequence 1.
    let prefetch_req = channels
        .loader_rx
        .try_recv()
        .expect("Expected prefetch LoaderRequest on loader_rx");
    assert_eq!(prefetch_req.idx, 1);
    assert!(prefetch_req.is_prefetch);
    assert_eq!(prefetch_req.sequence, 1);

    // No additional requests pending
    assert!(channels.loader_rx.try_recv().is_err());

    // Navigating to the next image triggers another start_load_image()
    app.next_image();
    assert_eq!(app.queue.current_index, 1);

    let next_active_req = channels
        .loader_rx
        .try_recv()
        .expect("Expected new active LoaderRequest after next_image()");
    assert_eq!(next_active_req.idx, 1);
    assert!(!next_active_req.is_prefetch);
    assert_eq!(next_active_req.sequence, 2);
}

#[test]
fn test_loader_response_thumbnail_progression_and_resize_dispatch() {
    let (mut app, channels) = setup_test_app_with_channels();

    // Drain initial loader requests
    while channels.loader_rx.try_recv().is_ok() {}

    // Set widget size so resize requests are generated
    app.last_widget_size = (80, 24);

    let thumb_img = DynamicImage::ImageRgb8(image::RgbImage::new(160, 120));
    let thumb_resp = LoaderResponse {
        idx: 0,
        is_prefetch: false,
        is_thumbnail: true,
        thumbnail_source: Some(crate::imaging::ThumbnailSource::Exif),
        sequence: app.current_sequence,
        result: Ok(DecodedImage {
            image: thumb_img,
            width: 160,
            height: 120,
            format: Some(image::ImageFormat::Jpeg),
            disk_size: 1024,
            raw_width: Some(1920),
            raw_height: Some(1080),
        }),
        decode_duration: std::time::Duration::from_millis(5),
    };

    // Send thumbnail response and process via check_background_responses
    channels.response_tx.send(thumb_resp).unwrap();
    assert!(app.check_background_responses());

    // Verify thumbnail state
    assert!(app.thumbnail_image.is_some());
    assert_eq!(app.img_width, 160);
    assert_eq!(app.img_height, 120);
    assert_eq!(
        app.stats.thumbnail_source,
        Some(crate::imaging::ThumbnailSource::Exif)
    );
    assert_eq!(app.stats.thumbnail_dimensions, Some((160, 120)));
    assert!(
        app.stats.load_duration.is_none(),
        "load_duration must be None during thumbnail placeholder phase"
    );

    // Verify ResizeRequest was triggered on resize_rx
    let resize_req = channels
        .resize_rx
        .try_recv()
        .expect("Expected ResizeRequest on resize_rx after thumbnail load");
    assert_eq!(resize_req.original_size, (160, 120));
    assert_eq!(resize_req.sequence, app.current_sequence);

    // Now simulate full-resolution load completion
    let full_img = DynamicImage::ImageRgb8(image::RgbImage::new(1920, 1080));
    let full_resp = LoaderResponse {
        idx: 0,
        is_prefetch: false,
        is_thumbnail: false,
        thumbnail_source: None,
        sequence: app.current_sequence,
        result: Ok(DecodedImage {
            image: full_img,
            width: 1920,
            height: 1080,
            format: Some(image::ImageFormat::Jpeg),
            disk_size: 20480,
            raw_width: Some(1920),
            raw_height: Some(1080),
        }),
        decode_duration: std::time::Duration::from_millis(25),
    };

    channels.response_tx.send(full_resp).unwrap();
    assert!(app.check_background_responses());

    // Verify full resolution state
    assert!(app.original_image.is_some());
    assert!(!app.show_thumbnail_only);
    assert!(!app.is_loading);
    assert_eq!(app.img_width, 1920);
    assert_eq!(app.img_height, 1080);
    assert_eq!(
        app.stats.load_duration,
        Some(std::time::Duration::from_millis(25))
    );

    // Verify updated ResizeRequest for full resolution
    let full_resize_req = channels
        .resize_rx
        .try_recv()
        .expect("Expected ResizeRequest on resize_rx after full resolution load");
    assert_eq!(full_resize_req.original_size, (1920, 1080));
}

#[test]
fn test_sequence_number_invalidation() {
    let (mut app, channels) = setup_test_app_with_channels();

    // Drain initial loader requests
    while channels.loader_rx.try_recv().is_ok() {}

    // Advance sequence
    app.current_sequence = 10;
    app.active_sequence
        .store(10, std::sync::atomic::Ordering::Release);

    // 1. Stale LoaderResponse with sequence 9 (< 10)
    let dummy_img = DynamicImage::ImageRgb8(image::RgbImage::new(100, 100));
    let stale_loader = LoaderResponse {
        idx: 0,
        is_prefetch: false,
        is_thumbnail: false,
        thumbnail_source: None,
        sequence: 9,
        result: Ok(DecodedImage {
            image: dummy_img,
            width: 100,
            height: 100,
            format: None,
            disk_size: 0,
            raw_width: None,
            raw_height: None,
        }),
        decode_duration: std::time::Duration::ZERO,
    };
    channels.response_tx.send(stale_loader).unwrap();
    app.check_background_responses();
    assert!(app.original_image.is_none());
    assert_eq!(app.img_width, 0);

    // 2. Stale ResizeResponse with sequence 9 (< 10)
    let stale_proto = ResizeResponse {
        protocol: app
            .picker
            .new_resize_protocol(DynamicImage::ImageRgb8(image::RgbImage::new(10, 10))),
        rendered_cells: (10, 10),
        process_duration: std::time::Duration::ZERO,
        protocol_duration: std::time::Duration::ZERO,
        target_width: 10,
        target_height: 10,
        sequence: 9,
    };
    channels.protocol_tx.send(stale_proto).unwrap();
    app.check_background_responses();
    assert!(app.image_protocol.is_none());

    // 3. Switching to an empty view mode (0 images match) increments current_sequence and active_sequence
    // and causes background responses for earlier sequences to be dropped
    let seq_before = app.current_sequence;
    let active_seq_before = app
        .active_sequence
        .load(std::sync::atomic::Ordering::Acquire);
    assert_eq!(seq_before, active_seq_before as u64);

    // No images are marked Pick, so ViewMode::Picks matches 0 images
    app.set_view_mode(ViewMode::Picks);
    assert_eq!(app.current_sequence, seq_before + 1);
    assert_eq!(
        app.active_sequence
            .load(std::sync::atomic::Ordering::Acquire),
        (seq_before + 1) as usize
    );
    assert_eq!(
        app.error_message.as_deref(),
        Some("No images match the current view filter")
    );

    // Stale in-flight response with sequence before the empty filter switch
    let stale_filter_resp = LoaderResponse {
        idx: 0,
        is_prefetch: false,
        is_thumbnail: false,
        thumbnail_source: None,
        sequence: seq_before,
        result: Ok(DecodedImage {
            image: DynamicImage::ImageRgb8(image::RgbImage::new(50, 50)),
            width: 50,
            height: 50,
            format: None,
            disk_size: 0,
            raw_width: None,
            raw_height: None,
        }),
        decode_duration: std::time::Duration::ZERO,
    };
    channels.response_tx.send(stale_filter_resp).unwrap();
    app.update_channels();
    assert!(app.original_image.is_none());
}

#[test]
fn test_decode_error_handling() {
    let (mut app, channels) = setup_test_app_with_channels();

    // Drain initial requests
    while channels.loader_rx.try_recv().is_ok() {}

    assert!(app.is_loading);
    assert!(app.error_message.is_none());

    // Send a decode error for the current active image
    let error_resp = LoaderResponse {
        idx: 0,
        is_prefetch: false,
        is_thumbnail: false,
        thumbnail_source: None,
        sequence: app.current_sequence,
        result: Err("Corrupt image data: decode failed".to_string()),
        decode_duration: std::time::Duration::from_millis(2),
    };

    channels.response_tx.send(error_resp).unwrap();
    app.check_background_responses();

    assert_eq!(
        app.error_message.as_deref(),
        Some("Corrupt image data: decode failed")
    );
    assert!(!app.is_loading);
    assert!(app.original_image.is_none());
}

#[test]
fn test_panning_safety_and_bounds() {
    let mut app = setup_test_app();

    // 1. Initial state
    assert_eq!(app.pan_offset.x, 0);
    assert_eq!(app.pan_offset.y, 0);

    // Mock a loaded image width and height
    app.img_width = 800;
    app.img_height = 600;
    app.is_loading = false;
    app.original_image = Some(std::sync::Arc::new(image::DynamicImage::ImageRgb8(
        image::RgbImage::new(800, 600),
    )));

    // Set last widget size to zero
    app.last_widget_size = (0, 0);
    let step_x = app.pan_step_x();
    let step_y = app.pan_step_y();
    // Since widget size is zero, s should be 0.0, which falls back to scale 1.0, and step_px should fall back to img_width * 0.05
    assert_eq!(step_x, (800.0 * 0.05) as i64);
    assert_eq!(step_y, (600.0 * 0.05) as i64);

    // 2. Extremely tiny scale or zero division checks
    app.last_widget_size = (10, 10);
    // Let's set zoom_factor extremely small
    app.zoom_factor = crate::imaging::types::ZoomFactor::new(0.0001);
    let step_x_large = app.pan_step_x();
    // The step is clamped to image width/height to avoid infinite or giant jumps
    assert!(step_x_large <= 800);
    assert!(step_x_large >= 1);

    // 3. Test saturating math in panning methods
    app.pan_offset.x = i64::MAX - 5;
    app.pan_right(); // should use saturating_add and then clamp back to max_pan_x (which is 800/2 = 400)
    assert_eq!(app.pan_offset.x, 400);

    app.pan_offset.x = i64::MIN + 5;
    app.pan_left(); // should use saturating_sub and then clamp back to -max_pan_x (-400)
    assert_eq!(app.pan_offset.x, -400);
}

fn setup_multi_image_app(count: usize) -> (App, MockChannels) {
    let images: Vec<ImageSource> = (1..=count)
        .map(|i| ImageSource::Local(PathBuf::from(format!("img{}.png", i))))
        .collect();
    let picker = Picker::halfblocks();
    let app_config = AppConfig {
        filter_type: crate::imaging::FilterType::Nearest,
        scale_mode: crate::imaging::ScaleMode::Shrink,
        disable_thumbnail: false,
        infobar: InfoBarPosition::Bottom,
        active_protocol: crate::config::ActiveProtocol::Halfblocks,
    };
    let scan_config = ScanConfig {
        initial_scan_path: PathBuf::from("."),
        check_magic: false,
        recursive: false,
        is_cbz: false,
        is_piped: false,
    };
    App::new_mocked(images, 0, picker, app_config, scan_config).unwrap()
}

#[test]
fn test_execute_prompt_goto_image() {
    let (mut app, _channels) = setup_multi_image_app(5);
    assert_eq!(app.queue.current_index, 0);

    // 1. Absolute 1-based index: "2" -> index 1
    app.palette_query.set("2");
    app.execute_prompt(PromptType::GoToImage);
    assert_eq!(app.queue.current_index, 1);
    assert_eq!(app.palette_mode, PaletteMode::Closed);
    assert_eq!(app.prompt_type, None);

    // 2. Absolute "0" -> ignored via checked_sub(1), stays at index 1
    app.palette_query.set("0");
    app.execute_prompt(PromptType::GoToImage);
    assert_eq!(app.queue.current_index, 1);

    // 3. Absolute "999" -> clamped to last visible index (4)
    app.palette_query.set("999");
    app.execute_prompt(PromptType::GoToImage);
    assert_eq!(app.queue.current_index, 4);

    // 4. Relative sub: "-1" from index 4 -> index 3
    app.palette_query.set("-1");
    app.execute_prompt(PromptType::GoToImage);
    assert_eq!(app.queue.current_index, 3);

    // 5. Relative add: "+1" from index 3 -> index 4
    app.palette_query.set("+1");
    app.execute_prompt(PromptType::GoToImage);
    assert_eq!(app.queue.current_index, 4);

    // 6. Filtered ViewMode (e.g. ViewMode::Picks where visible indices are non-contiguous)
    // Mark indices 1 and 3 as Pick, others as Reject
    app.queue.set_classification(0, Classification::Reject);
    app.queue.set_classification(1, Classification::Pick);
    app.queue.set_classification(2, Classification::Reject);
    app.queue.set_classification(3, Classification::Pick);
    app.queue.set_classification(4, Classification::Reject);
    app.view_mode = ViewMode::Picks;
    app.queue.current_index = 1; // First visible Pick

    // Absolute "2" in Picks mode -> 2nd visible Pick is index 3
    app.palette_query.set("2");
    app.execute_prompt(PromptType::GoToImage);
    assert_eq!(app.queue.current_index, 3);

    // Relative sub "-1" in Picks mode -> 1st visible Pick is index 1
    app.palette_query.set("-1");
    app.execute_prompt(PromptType::GoToImage);
    assert_eq!(app.queue.current_index, 1);

    // Relative add "+1" in Picks mode -> 2nd visible Pick is index 3
    app.palette_query.set("+1");
    app.execute_prompt(PromptType::GoToImage);
    assert_eq!(app.queue.current_index, 3);
}

#[test]
fn test_execute_prompt_brightness_and_contrast() {
    let (mut app, _channels) = setup_test_app_with_channels();

    // 1. When original_image is None, prompts early-return without changes
    assert!(app.original_image.is_none());
    app.palette_query.set("50");
    app.execute_prompt(PromptType::SetBrightness);
    assert_eq!(app.current_brightness().value(), 0);

    app.palette_query.set("=-25.5");
    app.execute_prompt(PromptType::SetContrast);
    assert_eq!(app.current_contrast().value(), 0.0);

    // 2. Set original_image to allow adjustments
    app.original_image = Some(std::sync::Arc::new(image::DynamicImage::ImageRgb8(
        image::RgbImage::new(100, 100),
    )));

    // Brightness: absolute "50"
    app.palette_query.set("50");
    app.execute_prompt(PromptType::SetBrightness);
    assert_eq!(app.current_brightness().value(), 50);

    // Brightness: relative add "+20" -> 70
    app.palette_query.set("+20");
    app.execute_prompt(PromptType::SetBrightness);
    assert_eq!(app.current_brightness().value(), 70);

    // Brightness: relative sub "-15" -> 55
    app.palette_query.set("-15");
    app.execute_prompt(PromptType::SetBrightness);
    assert_eq!(app.current_brightness().value(), 55);

    // Brightness: clamping upper bound (max 255)
    app.palette_query.set("+300");
    app.execute_prompt(PromptType::SetBrightness);
    assert_eq!(app.current_brightness().value(), 255);

    // Brightness: clamping lower bound (min -255)
    app.palette_query.set("-600");
    app.execute_prompt(PromptType::SetBrightness);
    assert_eq!(app.current_brightness().value(), -255);

    // Contrast: absolute "=-25.5"
    app.palette_query.set("=-25.5");
    app.execute_prompt(PromptType::SetContrast);
    assert!((app.current_contrast().value() - (-25.5)).abs() < 1e-4);

    // Contrast: relative add "+20" -> -5.5
    app.palette_query.set("+20");
    app.execute_prompt(PromptType::SetContrast);
    assert!((app.current_contrast().value() - (-5.5)).abs() < 1e-4);

    // Contrast: relative sub "-15" -> -20.5
    app.palette_query.set("-15");
    app.execute_prompt(PromptType::SetContrast);
    assert!((app.current_contrast().value() - (-20.5)).abs() < 1e-4);

    // Contrast: clamping upper bound (max 255.0)
    app.palette_query.set("+500");
    app.execute_prompt(PromptType::SetContrast);
    assert_eq!(app.current_contrast().value(), 255.0);

    // Contrast: clamping lower bound (min -255.0)
    app.palette_query.set("-600");
    app.execute_prompt(PromptType::SetContrast);
    assert_eq!(app.current_contrast().value(), -255.0);
}

#[test]
fn test_execute_prompt_slideshow() {
    let (mut app, _channels) = setup_test_app_with_channels();
    assert!(!app.slideshow_state.is_active());
    assert_eq!(app.slideshow_state.seconds(), 0);

    // 1. Absolute "5" -> Playing with 5s delay
    app.palette_query.set("5");
    app.execute_prompt(PromptType::SetSlideshow);
    assert!(app.slideshow_state.is_playing());
    assert_eq!(app.slideshow_state.seconds(), 5);

    // 2. Relative add "+3" -> 8s
    app.palette_query.set("+3");
    app.execute_prompt(PromptType::SetSlideshow);
    assert!(app.slideshow_state.is_playing());
    assert_eq!(app.slideshow_state.seconds(), 8);

    // 3. Relative sub "-8" -> 0s -> Transitions to Stopped
    app.palette_query.set("-8");
    app.execute_prompt(PromptType::SetSlideshow);
    assert!(!app.slideshow_state.is_active());
    assert_eq!(app.slideshow_state.seconds(), 0);
}

#[test]
fn test_zoom_stops_and_scale_modes() {
    let (mut app, _channels) = setup_test_app_with_channels();
    app.original_image = Some(std::sync::Arc::new(image::DynamicImage::ImageRgb8(
        image::RgbImage::new(800, 600),
    )));
    app.img_width = 800;
    app.img_height = 600;
    app.is_loading = false;
    app.last_widget_size = (100, 50); // With 8x16 font: 800x800 px viewport

    let fit_scale = app.get_fit_scale();
    assert!(fit_scale > 0.0);

    // 1. ScaleMode::None -> zoom_factor = 1.0 / fit_scale (displayed scale 1.0)
    app.scale_mode = ScaleMode::None;
    app.apply_scale_mode();
    assert!((app.zoom_factor.value() - (1.0 / fit_scale)).abs() < 1e-4);
    assert_eq!(app.pan_offset, PanOffset::ZERO);

    // 2. ScaleMode::Shrink with image larger than viewport (fit_scale < 1.0)
    // Set viewport smaller than image: (50, 20) -> 400x320 px
    app.last_widget_size = (50, 20);
    let s_large = app.get_fit_scale();
    assert!(s_large < 1.0);
    app.scale_mode = ScaleMode::Shrink;
    app.apply_scale_mode();
    assert_eq!(app.zoom_factor.value(), 1.0);

    // ScaleMode::Shrink with image smaller than viewport (fit_scale >= 1.0)
    // Reset viewport back to (200, 100) -> 1600x1600 px
    app.last_widget_size = (200, 100);
    let s_small = app.get_fit_scale();
    assert!(s_small >= 1.0);
    app.scale_mode = ScaleMode::Shrink;
    app.apply_scale_mode();
    assert!((app.zoom_factor.value() - (1.0 / s_small)).abs() < 1e-4);

    // 3. ScaleMode::Full -> zoom_factor = 1.0
    app.scale_mode = ScaleMode::Full;
    app.apply_scale_mode();
    assert_eq!(app.zoom_factor.value(), 1.0);

    // 4. ScaleMode::Crop -> zoom_factor = s_w.max(s_h) / fit_scale
    app.scale_mode = ScaleMode::Crop;
    app.apply_scale_mode();
    assert!(app.zoom_factor.value() >= 1.0);

    // 5. cycle_scale_mode transitions: None -> Shrink -> Full -> Crop -> None
    app.scale_mode = ScaleMode::None;
    app.cycle_scale_mode();
    assert_eq!(app.scale_mode, ScaleMode::Shrink);
    app.cycle_scale_mode();
    assert_eq!(app.scale_mode, ScaleMode::Full);
    app.cycle_scale_mode();
    assert_eq!(app.scale_mode, ScaleMode::Crop);
    app.cycle_scale_mode();
    assert_eq!(app.scale_mode, ScaleMode::None);

    // 6. jump_zoom_in and jump_zoom_out monotonic progression
    app.last_widget_size = (100, 50);
    let fs = app.get_fit_scale();
    // Start from very small zoom
    app.zoom_factor = ZoomFactor::new(0.01 / fs);
    let mut prev_scale = fs * app.zoom_factor.value();

    // Step up with jump_zoom_in across predefined stops and beyond
    for _ in 0..10 {
        app.jump_zoom_in();
        let curr_scale = fs * app.zoom_factor.value();
        assert!(
            curr_scale >= prev_scale,
            "jump_zoom_in must be monotonically increasing"
        );
        prev_scale = curr_scale;
    }
    // At maximum limit, clamped at 102.4
    let max_scale = fs * app.zoom_factor.value();
    assert!((max_scale - 102.4).abs() < 0.1 || max_scale <= 102.4);

    // Step down with jump_zoom_out
    for _ in 0..15 {
        app.jump_zoom_out();
        let curr_scale = fs * app.zoom_factor.value();
        assert!(
            curr_scale <= prev_scale,
            "jump_zoom_out must be monotonically decreasing"
        );
        prev_scale = curr_scale;
    }
    // At minimum limit, clamped at 0.01
    let min_scale = fs * app.zoom_factor.value();
    assert!((min_scale - 0.01).abs() < 0.005 || min_scale >= 0.01);

    // 7. set_actual_size, reset_view, and reset_image
    app.zoom_factor = ZoomFactor::new(5.0);
    app.pan_offset = PanOffset::new(100, -50);
    app.clear_on_protocol_receive = false;
    app.set_actual_size();
    assert!((app.zoom_factor.value() - (1.0 / fs)).abs() < 1e-4);
    assert!(app.clear_on_protocol_receive);

    app.zoom_factor = ZoomFactor::new(3.0);
    app.pan_offset = PanOffset::new(80, 40);
    app.clear_on_protocol_receive = false;
    app.reset_view();
    assert_eq!(app.pan_offset, PanOffset::ZERO);
    assert!(app.clear_on_protocol_receive);

    // reset_image resets adjustments on current index
    let curr = app.queue.current_index;
    app.adjustments_mut()[curr].rotation = Rotation::D90;
    app.adjustments_mut()[curr].brightness = Brightness::new(40);
    app.adjustments_mut()[curr].contrast = Contrast::new(30.0);
    app.reset_image();
    assert_eq!(app.adjustments()[curr].rotation, Rotation::D0);
    assert_eq!(app.adjustments()[curr].brightness, Brightness::ZERO);
    assert_eq!(app.adjustments()[curr].contrast, Contrast::ZERO);
}

#[test]
fn test_should_clear_on_update_and_dismiss_needs_clear() {
    let (mut app, _channels) = setup_test_app_with_channels();

    // 1. Check ProtocolType gating on should_clear_on_update
    app.picker.set_protocol_type(ProtocolType::Halfblocks);
    assert!(!app.should_clear_on_update());

    app.picker.set_protocol_type(ProtocolType::Kitty);
    assert!(!app.should_clear_on_update());

    app.picker.set_protocol_type(ProtocolType::Iterm2);
    assert!(!app.should_clear_on_update());

    app.picker.set_protocol_type(ProtocolType::Sixel);
    assert!(app.should_clear_on_update());

    // 2. Overlay dismissal sets needs_clear = true
    // Open and close prompt
    app.needs_clear = false;
    app.open_prompt(PromptType::GoToImage);
    assert!(app.needs_clear);

    app.needs_clear = false;
    app.execute_prompt(PromptType::GoToImage);
    assert!(app.needs_clear);

    // Open and close Info overlay
    app.needs_clear = false;
    app.execute_command(Command::ShowInfo);
    assert!(app.needs_clear);
    assert_eq!(app.palette_mode, PaletteMode::Info);

    app.needs_clear = false;
    app.last_info_toggle = Some(std::time::Instant::now() - std::time::Duration::from_millis(300));
    app.execute_command(Command::ShowInfo);
    assert!(app.needs_clear);
    assert_eq!(app.palette_mode, PaletteMode::Closed);

    // Slideshow pause toggle sets needs_clear
    app.needs_clear = false;
    app.execute_command(Command::ToggleSlideshowPause);
    assert!(app.needs_clear);
}

#[test]
fn test_fast_scroll_prefetch_throttling_and_tick() {
    let (mut app, channels) = setup_test_app_with_channels();
    // Drain initial requests from setup
    while channels.loader_rx.try_recv().is_ok() {}

    // 1. Initial state
    assert!(!app.prefetch_suspended);
    assert_eq!(
        app.active_sequence.load(Ordering::Acquire),
        app.current_sequence as usize
    );

    // 2. Trigger first navigation (isolated after threshold)
    app.last_nav_time = Some(Instant::now() - Duration::from_millis(100));
    app.next_image();
    assert!(!app.prefetch_suspended);
    // Drain requests
    let mut reqs = Vec::new();
    while let Ok(r) = channels.loader_rx.try_recv() {
        reqs.push(r);
    }
    assert!(reqs.iter().any(|r| !r.is_prefetch));

    // 3. Trigger rapid successive navigation (within FAST_SCROLL_THRESHOLD)
    app.next_image();
    assert!(app.prefetch_suspended);
    assert_eq!(
        app.active_sequence.load(Ordering::Acquire),
        app.current_sequence as usize
    );

    // In fast scroll, active load is sent, but prefetch requests are skipped
    let mut rapid_reqs = Vec::new();
    while let Ok(r) = channels.loader_rx.try_recv() {
        rapid_reqs.push(r);
    }
    // Active load was sent
    assert_eq!(rapid_reqs.len(), 1);
    assert!(!rapid_reqs[0].is_prefetch);

    // 4. Tick before threshold does not resume prefetch
    app.tick();
    assert!(app.prefetch_suspended);
    assert!(channels.loader_rx.try_recv().is_err());

    // 5. Once elapsed time exceeds threshold, tick resumes prefetch
    app.last_nav_time = Some(Instant::now() - Duration::from_millis(70));
    app.tick();
    assert!(!app.prefetch_suspended);

    // Prefetch requests were dispatched by tick
    let mut resumed_reqs = Vec::new();
    while let Ok(r) = channels.loader_rx.try_recv() {
        resumed_reqs.push(r);
    }
    assert!(resumed_reqs.iter().any(|r| r.is_prefetch));
}

#[test]
fn test_prefetch_cache_stats_and_thumbnail_sources() {
    let app = setup_test_app();
    let (count, bytes) = app.prefetch_cache_stats();
    assert_eq!(count, 0);
    assert_eq!(bytes, 0);

    // Insert cached image with image (10x10) and thumbnail (5x5)
    let img = Arc::new(DynamicImage::ImageRgb8(image::RgbImage::new(10, 10)));
    let thumb = Arc::new(DynamicImage::ImageRgb8(image::RgbImage::new(5, 5)));
    {
        let mut cache = app.prefetch_cache.lock().unwrap();
        cache.insert(
            0,
            CachedImage {
                image: Some(img),
                thumbnail: Some(thumb),
                thumbnail_source: Some(crate::imaging::ThumbnailSource::XdgCache),
                width: 10,
                height: 10,
                format: Some(image::ImageFormat::Png),
                decode_duration: std::time::Duration::from_millis(10),
                thumbnail_decode_duration: std::time::Duration::from_millis(2),
                disk_size: 500,
                raw_width: None,
                raw_height: None,
            },
        );
    }

    let (count, bytes) = app.prefetch_cache_stats();
    assert_eq!(count, 1);
    // 10*10*3 + 5*5*3 = 300 + 75 = 375 bytes
    assert_eq!(bytes, 375);

    // Test thumbnail source label
    assert_eq!(
        crate::imaging::ThumbnailSource::Exif.display_label(),
        "EXIF"
    );
    assert_eq!(
        crate::imaging::ThumbnailSource::XdgCache.display_label(),
        "XDG"
    );
}
