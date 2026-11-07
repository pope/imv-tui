//! CPU-bound image scaling, panning, and resizing worker pools.

use fast_image_resize as fir;
use image::{DynamicImage, GenericImage};
use ratatui_image::protocol::StatefulProtocol;

use crate::imaging::ResizeRequest;
use crate::imaging::types::*;

/// Helper resizing function leveraging fast_image_resize for sub-millisecond execution speeds.
pub fn fast_resize(
    resizer: &mut fir::Resizer,
    src_img: &DynamicImage,
    dst_w: u32,
    dst_h: u32,
    filter_type: FilterType,
    crop_rect: Option<(f64, f64, f64, f64)>,
) -> Result<DynamicImage, Box<dyn std::error::Error>> {
    use fast_image_resize::images::Image as FirImage;

    let resize_alg = match filter_type {
        FilterType::Nearest => fir::ResizeAlg::Nearest,
        FilterType::Linear => fir::ResizeAlg::Convolution(fir::FilterType::Bilinear),
        FilterType::Cubic => fir::ResizeAlg::Convolution(fir::FilterType::CatmullRom),
        FilterType::Mitchell => fir::ResizeAlg::Convolution(fir::FilterType::Mitchell),
        FilterType::Gaussian => fir::ResizeAlg::Convolution(fir::FilterType::Gaussian),
        FilterType::Lanczos => fir::ResizeAlg::Convolution(fir::FilterType::Lanczos3),
        FilterType::Hamming => fir::ResizeAlg::Convolution(fir::FilterType::Hamming),
    };

    let temp_rgba;
    let rgba_src = match src_img {
        DynamicImage::ImageRgba8(rgba) => rgba,
        other => {
            temp_rgba = other.to_rgba8();
            &temp_rgba
        }
    };

    let mut dst_image = FirImage::new(dst_w, dst_h, fir::PixelType::U8x4);

    let mut options = fir::ResizeOptions::new();
    options.algorithm = resize_alg;
    if let Some((left, top, width, height)) = crop_rect {
        options = options.crop(left, top, width, height);
    }

    resizer.resize(rgba_src, &mut dst_image, Some(&options))?;

    let buffer = dst_image.into_vec();
    let rgba_dst = image::RgbaImage::from_raw(dst_w, dst_h, buffer)
        .ok_or("Failed to construct RgbaImage from resized buffer")?;
    Ok(DynamicImage::ImageRgba8(rgba_dst))
}

/// Renders the scaled and panned image onto the destination canvas, applying
/// offscreen boundary margins, centering, and brightness/contrast adjustments.
pub(crate) fn render_resize_canvas(
    req: &ResizeRequest,
    resizer: &mut fir::Resizer,
) -> DynamicImage {
    // Map input crop/intersection coordinates from the full image space to the thumbnail space
    // if the loaded image buffer is a thumbnail placeholder of different dimensions.
    let (scale, crop, intersection) =
        if req.img.width() != req.original_size.0 || req.img.height() != req.original_size.1 {
            let factor_x = req.img.width() as f64 / req.original_size.0 as f64;
            let factor_y = req.img.height() as f64 / req.original_size.1 as f64;

            let crop_x1 = (req.crop.x1 as f64 * factor_x).round() as i64;
            let crop_y1 = (req.crop.y1 as f64 * factor_y).round() as i64;
            let crop_x2 = (req.crop.x2 as f64 * factor_x).round() as i64;
            let crop_y2 = (req.crop.y2 as f64 * factor_y).round() as i64;
            let scaled_crop = CropBox::new(crop_x1, crop_y1, crop_x2, crop_y2);

            let inter_x1 =
                ((req.intersection.x1 as f64 * factor_x).round() as u32).min(req.img.width());
            let inter_y1 =
                ((req.intersection.y1 as f64 * factor_y).round() as u32).min(req.img.height());
            let inter_x2 =
                ((req.intersection.x2 as f64 * factor_x).round() as u32).min(req.img.width());
            let inter_y2 =
                ((req.intersection.y2 as f64 * factor_y).round() as u32).min(req.img.height());
            let scaled_inter = ImageIntersection::new(inter_x1, inter_y1, inter_x2, inter_y2);

            let new_scale = req.target_w as f64 / scaled_crop.width().max(1) as f64;

            (new_scale, scaled_crop, scaled_inter)
        } else {
            (req.scale, req.crop, req.intersection)
        };
    let img_to_resize: &DynamicImage = &req.img;

    let mut canvas = if intersection.x1 as i64 == crop.x1
        && intersection.x2 as i64 == crop.x2
        && intersection.y1 as i64 == crop.y1
        && intersection.y2 as i64 == crop.y2
    {
        let crop_rect = Some((
            intersection.x1 as f64,
            intersection.y1 as f64,
            intersection.width() as f64,
            intersection.height() as f64,
        ));
        match fast_resize(
            resizer,
            img_to_resize,
            req.target_w,
            req.target_h,
            req.filter_type,
            crop_rect,
        ) {
            Ok(resized) => resized,
            Err(_) => {
                let cropped_part = img_to_resize.crop_imm(
                    intersection.x1,
                    intersection.y1,
                    intersection.width(),
                    intersection.height(),
                );
                cropped_part.resize(
                    req.target_w,
                    req.target_h,
                    req.filter_type.to_image_filter(),
                )
            }
        }
    } else {
        let mut screen_canvas = image::RgbaImage::new(req.target_w, req.target_h);

        if !intersection.is_empty() {
            let target_inter_w = ((intersection.width() as f64 * scale).round() as u32).max(1);
            let target_inter_h = ((intersection.height() as f64 * scale).round() as u32).max(1);

            let crop_rect = Some((
                intersection.x1 as f64,
                intersection.y1 as f64,
                intersection.width() as f64,
                intersection.height() as f64,
            ));

            let resized_part = match fast_resize(
                resizer,
                img_to_resize,
                target_inter_w,
                target_inter_h,
                req.filter_type,
                crop_rect,
            ) {
                Ok(resized) => resized,
                Err(_) => {
                    let cropped_part = img_to_resize.crop_imm(
                        intersection.x1,
                        intersection.y1,
                        intersection.width(),
                        intersection.height(),
                    );
                    cropped_part.resize(
                        target_inter_w,
                        target_inter_h,
                        req.filter_type.to_image_filter(),
                    )
                }
            };

            let paste_x = ((intersection.x1 as i64 - crop.x1) as f64 * scale).round() as i64;
            let paste_y = ((intersection.y1 as i64 - crop.y1) as f64 * scale).round() as i64;

            let paste_x =
                paste_x.clamp(0, (req.target_w as i64 - target_inter_w as i64).max(0)) as u32;
            let paste_y =
                paste_y.clamp(0, (req.target_h as i64 - target_inter_h as i64).max(0)) as u32;

            let copy_w = target_inter_w.min(req.target_w.saturating_sub(paste_x));
            let copy_h = target_inter_h.min(req.target_h.saturating_sub(paste_y));

            if copy_w > 0 && copy_h > 0 {
                let part_to_copy = if copy_w < target_inter_w || copy_h < target_inter_h {
                    resized_part.crop_imm(0, 0, copy_w, copy_h)
                } else {
                    resized_part
                };

                if let Some(rgba_part) = part_to_copy.as_rgba8() {
                    let _ = screen_canvas.copy_from(rgba_part, paste_x, paste_y);
                } else {
                    let _ = screen_canvas.copy_from(&part_to_copy.to_rgba8(), paste_x, paste_y);
                }
            }
        }
        DynamicImage::ImageRgba8(screen_canvas)
    };

    if let Some(rgba_canvas) = canvas.as_mut_rgba8() {
        if !req.brightness.is_zero() {
            image::imageops::colorops::brighten_in_place(rgba_canvas, req.brightness.value());
        }
        if !req.contrast.is_zero() {
            image::imageops::colorops::contrast_in_place(rgba_canvas, req.contrast.value());
        }
    }

    canvas
}

/// Processes a scaling and panning request in the background, creating/rendering
/// the final scaled viewport on a screen-pixel canvas block to support offscreen panning boundaries.
pub fn process_resize(
    req: ResizeRequest,
    resizer: &mut fir::Resizer,
) -> (StatefulProtocol, std::time::Duration, std::time::Duration) {
    let start_process = std::time::Instant::now();
    let canvas = render_resize_canvas(&req, resizer);
    let process_duration = start_process.elapsed();

    let start_protocol = std::time::Instant::now();
    let protocol = req.picker.new_resize_protocol(canvas);
    let protocol_duration = start_protocol.elapsed();

    (protocol, process_duration, protocol_duration)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};
    use ratatui_image::picker::Picker;
    use std::sync::Arc;

    fn make_test_image(width: u32, height: u32) -> DynamicImage {
        let mut img = RgbaImage::new(width, height);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            *pixel = Rgba([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8, 255]);
        }
        DynamicImage::ImageRgba8(img)
    }

    fn make_gray_test_image(width: u32, height: u32, val: u8) -> DynamicImage {
        let mut img = RgbaImage::new(width, height);
        for pixel in img.pixels_mut() {
            *pixel = Rgba([val, val, val, 255]);
        }
        DynamicImage::ImageRgba8(img)
    }

    #[test]
    fn test_fast_resize_filters_downscale_and_upscale() {
        let mut resizer = fir::Resizer::new();
        let src = make_test_image(100, 100);

        let filters = [
            FilterType::Nearest,
            FilterType::Linear,
            FilterType::Cubic,
            FilterType::Mitchell,
            FilterType::Gaussian,
            FilterType::Lanczos,
            FilterType::Hamming,
        ];

        for filter in filters {
            // Downscale
            let downscaled =
                fast_resize(&mut resizer, &src, 40, 30, filter, None).expect("Downscale failed");
            assert_eq!(downscaled.width(), 40);
            assert_eq!(downscaled.height(), 30);

            // Upscale
            let upscaled =
                fast_resize(&mut resizer, &src, 200, 150, filter, None).expect("Upscale failed");
            assert_eq!(upscaled.width(), 200);
            assert_eq!(upscaled.height(), 150);
        }
    }

    #[test]
    fn test_fast_resize_with_crop() {
        let mut resizer = fir::Resizer::new();
        let src = make_test_image(100, 100);

        let crop = Some((20.0, 20.0, 40.0, 40.0));
        let resized = fast_resize(&mut resizer, &src, 80, 80, FilterType::Lanczos, crop)
            .expect("Crop resize failed");
        assert_eq!(resized.width(), 80);
        assert_eq!(resized.height(), 80);
    }

    #[test]
    fn test_process_resize_fully_contained() {
        let mut resizer = fir::Resizer::new();
        let src = Arc::new(make_test_image(100, 100));
        let picker = Picker::halfblocks();

        let req = ResizeRequest {
            img: src,
            original_size: (100, 100),
            scale: 0.5,
            crop: CropBox::new(0, 0, 100, 100),
            intersection: ImageIntersection::new(0, 0, 100, 100),
            target_w: 50,
            target_h: 50,
            filter_type: FilterType::Nearest,
            picker,
            brightness: Brightness::ZERO,
            contrast: Contrast::ZERO,
            rendered_size_cells: (25, 25),
            sequence: 1,
        };

        let canvas = render_resize_canvas(&req, &mut resizer);
        assert_eq!(canvas.width(), 50);
        assert_eq!(canvas.height(), 50);
        let rgba = canvas.to_rgba8();
        assert_eq!(rgba.get_pixel(0, 0)[3], 255);
        assert_eq!(rgba.get_pixel(25, 25)[3], 255);
        assert_eq!(rgba.get_pixel(49, 49)[3], 255);

        let (_proto, proc_dur, _proto_dur) = process_resize(req, &mut resizer);
        assert!(proc_dur.as_nanos() > 0 || proc_dur.as_secs() == 0);
    }

    #[test]
    fn test_process_resize_canvas_centering_and_margins() {
        let mut resizer = fir::Resizer::new();
        let src = Arc::new(make_test_image(40, 40));
        let picker = Picker::halfblocks();

        // Image is 40x40, target viewport is 100x100.
        // Crop box extends beyond the image bounds (margins).
        let req = ResizeRequest {
            img: src,
            original_size: (40, 40),
            scale: 1.0,
            crop: CropBox::new(-30, -30, 70, 70),
            intersection: ImageIntersection::new(0, 0, 40, 40),
            target_w: 100,
            target_h: 100,
            filter_type: FilterType::Nearest,
            picker,
            brightness: Brightness::ZERO,
            contrast: Contrast::ZERO,
            rendered_size_cells: (50, 50),
            sequence: 1,
        };

        let canvas = render_resize_canvas(&req, &mut resizer);
        assert_eq!(canvas.width(), 100);
        assert_eq!(canvas.height(), 100);

        let rgba = canvas.to_rgba8();
        // Margins outside the centered 40x40 area (which is pasted at x=30..70, y=30..70)
        // must remain completely transparent
        assert_eq!(*rgba.get_pixel(0, 0), Rgba([0, 0, 0, 0]));
        assert_eq!(*rgba.get_pixel(99, 99), Rgba([0, 0, 0, 0]));
        assert_eq!(*rgba.get_pixel(29, 29), Rgba([0, 0, 0, 0]));
        assert_eq!(*rgba.get_pixel(70, 70), Rgba([0, 0, 0, 0]));

        // Pixels inside the pasted area must be opaque (alpha == 255)
        assert_eq!(rgba.get_pixel(30, 30)[3], 255);
        assert_eq!(rgba.get_pixel(50, 50)[3], 255);
        assert_eq!(rgba.get_pixel(69, 69)[3], 255);

        let (_proto, _proc_dur, _proto_dur) = process_resize(req, &mut resizer);
    }

    #[test]
    fn test_process_resize_pan_offset_boundary() {
        let mut resizer = fir::Resizer::new();
        let src = Arc::new(make_test_image(80, 80));
        let picker = Picker::halfblocks();

        // Panned offset resulting in negative crop coordinates and partial intersection
        let req = ResizeRequest {
            img: src,
            original_size: (80, 80),
            scale: 1.0,
            crop: CropBox::new(-20, -20, 60, 60),
            intersection: ImageIntersection::new(0, 0, 60, 60),
            target_w: 80,
            target_h: 80,
            filter_type: FilterType::Linear,
            picker,
            brightness: Brightness::ZERO,
            contrast: Contrast::ZERO,
            rendered_size_cells: (40, 40),
            sequence: 1,
        };

        let canvas = render_resize_canvas(&req, &mut resizer);
        assert_eq!(canvas.width(), 80);
        assert_eq!(canvas.height(), 80);

        let rgba = canvas.to_rgba8();
        // Unpasted region at (0, 0) must be transparent
        assert_eq!(*rgba.get_pixel(0, 0), Rgba([0, 0, 0, 0]));
        assert_eq!(*rgba.get_pixel(19, 19), Rgba([0, 0, 0, 0]));

        // Panned region starting at (20, 20) must be opaque
        assert_eq!(rgba.get_pixel(20, 20)[3], 255);
        assert_eq!(rgba.get_pixel(25, 25)[3], 255);
        assert_eq!(rgba.get_pixel(79, 79)[3], 255);

        let (_proto, _proc_dur, _proto_dur) = process_resize(req, &mut resizer);
    }

    #[test]
    fn test_process_resize_brightness_and_contrast() {
        let mut resizer = fir::Resizer::new();
        let src = Arc::new(make_gray_test_image(60, 60, 100));
        let picker = Picker::halfblocks();

        // Test positive brightness and contrast adjustments
        let req_pos = ResizeRequest {
            img: Arc::clone(&src),
            original_size: (60, 60),
            scale: 1.0,
            crop: CropBox::new(0, 0, 60, 60),
            intersection: ImageIntersection::new(0, 0, 60, 60),
            target_w: 60,
            target_h: 60,
            filter_type: FilterType::Nearest,
            picker: picker.clone(),
            brightness: Brightness::new(30),
            contrast: Contrast::new(20.0),
            rendered_size_cells: (30, 30),
            sequence: 1,
        };
        let canvas_pos = render_resize_canvas(&req_pos, &mut resizer);
        assert_eq!(canvas_pos.width(), 60);
        assert_eq!(canvas_pos.height(), 60);
        let rgba_pos = canvas_pos.to_rgba8();
        let px_pos = rgba_pos.get_pixel(30, 30);
        assert!(
            px_pos[0] > 100 && px_pos[1] > 100 && px_pos[2] > 100,
            "Expected RGB channels > 100 with positive brightness and contrast, got {:?}",
            px_pos
        );
        assert!(px_pos[3] > 0);
        let (_proto, _proc_dur, _proto_dur) = process_resize(req_pos, &mut resizer);

        // Test negative brightness and contrast adjustments
        let req_neg = ResizeRequest {
            img: src,
            original_size: (60, 60),
            scale: 1.0,
            crop: CropBox::new(0, 0, 60, 60),
            intersection: ImageIntersection::new(0, 0, 60, 60),
            target_w: 60,
            target_h: 60,
            filter_type: FilterType::Nearest,
            picker,
            brightness: Brightness::new(-30),
            contrast: Contrast::new(-20.0),
            rendered_size_cells: (30, 30),
            sequence: 2,
        };
        let canvas_neg = render_resize_canvas(&req_neg, &mut resizer);
        assert_eq!(canvas_neg.width(), 60);
        assert_eq!(canvas_neg.height(), 60);
        let rgba_neg = canvas_neg.to_rgba8();
        let px_neg = rgba_neg.get_pixel(30, 30);
        assert!(
            px_neg[0] < 100 && px_neg[1] < 100 && px_neg[2] < 100,
            "Expected RGB channels < 100 with negative brightness and contrast, got {:?}",
            px_neg
        );
        assert!(px_neg[3] > 0);
        let (_proto, _proc_dur, _proto_dur) = process_resize(req_neg, &mut resizer);
    }
}
