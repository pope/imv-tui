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

    let rgba_src = crate::imaging::to_rgba8_fast(src_img);

    let mut dst_image = FirImage::new(dst_w, dst_h, fir::PixelType::U8x4);

    let mut options = fir::ResizeOptions::new();
    options.algorithm = resize_alg;
    if let Some((left, top, width, height)) = crop_rect {
        options = options.crop(left, top, width, height);
    }

    resizer.resize(rgba_src.as_ref(), &mut dst_image, Some(&options))?;

    let buffer = dst_image.into_vec();
    let mut rgba_dst = image::RgbaImage::from_raw(dst_w, dst_h, buffer)
        .ok_or("Failed to construct RgbaImage from resized buffer")?;

    let (crop_w, crop_h) = match crop_rect {
        Some((_, _, w, h)) => (w, h),
        None => (src_img.width() as f64, src_img.height() as f64),
    };
    let is_magnified = dst_w as f64 >= crop_w && dst_h as f64 >= crop_h;

    if is_magnified
        && matches!(
            filter_type,
            FilterType::Lanczos | FilterType::Mitchell | FilterType::Cubic
        )
    {
        clamp_anti_ringing_rgba(&mut rgba_dst, &rgba_src, crop_rect);
    }

    Ok(DynamicImage::ImageRgba8(rgba_dst))
}

/// Renders the scaled and panned image onto the destination canvas, applying
/// offscreen boundary margins, centering, and brightness/contrast adjustments.
pub fn render_resize_canvas(req: &ResizeRequest, resizer: &mut fir::Resizer) -> DynamicImage {
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
            apply_brightness_simd(rgba_canvas.as_mut(), req.brightness.value());
        }
        if !req.contrast.is_zero() {
            apply_contrast_lut(rgba_canvas.as_mut(), req.contrast.value());
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
    let rendered_cells = req.rendered_size_cells;
    let start_process = std::time::Instant::now();
    let canvas = render_resize_canvas(&req, resizer);
    let process_duration = start_process.elapsed();

    let start_protocol = std::time::Instant::now();
    let mut protocol = req.picker.new_resize_protocol(canvas);
    let target_size = ratatui::layout::Size::new(rendered_cells.0, rendered_cells.1);
    ratatui_image::ResizeEncodeRender::resize_encode(
        &mut protocol,
        &ratatui_image::Resize::Crop(None),
        target_size,
    );
    let protocol_duration = start_protocol.elapsed();

    (protocol, process_duration, protocol_duration)
}

/// Applies brightness adjustment to RGBA pixel buffer in-place while strictly preserving the Alpha channel.
pub fn apply_brightness_simd(pixels: &mut [u8], value: i32) {
    if value == 0 {
        return;
    }
    if value > 0 {
        let delta = value.min(255) as u8;
        for chunk in pixels.as_chunks_mut::<4>().0 {
            chunk[0] = chunk[0].saturating_add(delta);
            chunk[1] = chunk[1].saturating_add(delta);
            chunk[2] = chunk[2].saturating_add(delta);
        }
    } else {
        let delta = (-value).min(255) as u8;
        for chunk in pixels.as_chunks_mut::<4>().0 {
            chunk[0] = chunk[0].saturating_sub(delta);
            chunk[1] = chunk[1].saturating_sub(delta);
            chunk[2] = chunk[2].saturating_sub(delta);
        }
    }
}

/// Numerically stable continuous rational highlight soft-knee curve from `peng`.
///
/// Smoothly compresses highlight values above `knee = 0.75` towards an asymptote of `1.0`,
/// preventing harsh clipping transitions while ensuring $C^1$ continuity and complete
/// numerical stability (free of NaN, Inf, or overflow even for extreme inputs).
#[inline]
pub fn soft_knee_highlight(x: f32) -> f32 {
    let knee = 0.75f32;
    let w = 0.25f32;
    if x <= knee {
        x.max(0.0)
    } else {
        let excess = x - knee;
        knee + (w * excess) / (excess + w)
    }
}

/// Applies contrast adjustment to an RGBA pixel buffer using a 256-byte stack LUT incorporating
/// `peng`'s photometric contrast curve and soft-knee shoulder compression while preserving Alpha.
pub fn apply_contrast_lut(pixels: &mut [u8], contrast: f32) {
    if contrast == 0.0 {
        return;
    }

    let factor = ((100.0 + contrast) / 100.0).max(0.0);
    let factor = factor * factor;

    let mut lut = [0u8; 256];
    for (i, slot) in lut.iter_mut().enumerate() {
        let normalized = i as f32 / 255.0;
        let mut val = (normalized - 0.5) * factor + 0.5;
        if contrast > 0.0 {
            val = soft_knee_highlight(val);
        }
        *slot = (val.clamp(0.0, 1.0) * 255.0).round() as u8;
    }

    for chunk in pixels.as_chunks_mut::<4>().0 {
        chunk[0] = lut[chunk[0] as usize];
        chunk[1] = lut[chunk[1] as usize];
        chunk[2] = lut[chunk[2] as usize];
    }
}

/// Clamps destination pixel colors to the min/max bounding envelope of the corresponding
/// 2x2 source texel neighborhood to eliminate Gibbs ringing/overshoot artifacts from lobed filters.
pub fn clamp_anti_ringing_rgba(
    dst: &mut image::RgbaImage,
    src: &image::RgbaImage,
    crop_rect: Option<(f64, f64, f64, f64)>,
) {
    let dst_w = dst.width();
    let dst_h = dst.height();
    let src_w = src.width();
    let src_h = src.height();

    if dst_w == 0 || dst_h == 0 || src_w == 0 || src_h == 0 {
        return;
    }

    let (crop_left, crop_top, crop_w, crop_h) =
        crop_rect.unwrap_or((0.0, 0.0, src_w as f64, src_h as f64));
    let scale_x = crop_w / dst_w as f64;
    let scale_y = crop_h / dst_h as f64;

    if scale_x > 1.0 || scale_y > 1.0 {
        return;
    }

    for y in 0..dst_h {
        let src_y_f = crop_top + (y as f64 + 0.5) * scale_y - 0.5;
        let y0 = (src_y_f.floor() as i64).clamp(0, src_h as i64 - 1) as u32;
        let y1 = (y0 + 1).min(src_h - 1);

        for x in 0..dst_w {
            let src_x_f = crop_left + (x as f64 + 0.5) * scale_x - 0.5;
            let x0 = (src_x_f.floor() as i64).clamp(0, src_w as i64 - 1) as u32;
            let x1 = (x0 + 1).min(src_w - 1);

            let p00 = src.get_pixel(x0, y0).0;
            let p10 = src.get_pixel(x1, y0).0;
            let p01 = src.get_pixel(x0, y1).0;
            let p11 = src.get_pixel(x1, y1).0;

            let dst_pixel = dst.get_pixel_mut(x, y);

            for c in 0..3 {
                let min_c = p00[c].min(p10[c]).min(p01[c]).min(p11[c]);
                let max_c = p00[c].max(p10[c]).max(p01[c]).max(p11[c]);
                dst_pixel.0[c] = dst_pixel.0[c].clamp(min_c, max_c);
            }
        }
    }
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
            rendered_size_cells: (5, 3),
            sequence: 1,
        };

        let canvas = render_resize_canvas(&req, &mut resizer);
        assert_eq!(canvas.width(), 50);
        assert_eq!(canvas.height(), 50);
        let rgba = canvas.to_rgba8();
        assert_eq!(rgba.get_pixel(0, 0)[3], 255);
        assert_eq!(rgba.get_pixel(25, 25)[3], 255);
        assert_eq!(rgba.get_pixel(49, 49)[3], 255);

        let (proto, proc_dur, proto_dur) = process_resize(req, &mut resizer);
        assert!(proc_dur.as_nanos() > 0 || proc_dur.as_secs() == 0);
        assert!(proto_dur.as_nanos() > 0 || proto_dur.as_secs() == 0);
        let target_size = ratatui::layout::Size::new(5, 3);
        assert!(
            ratatui_image::ResizeEncodeRender::needs_resize(
                &proto,
                &ratatui_image::Resize::Crop(None),
                target_size
            )
            .is_none(),
            "Expected protocol to be pre-encoded for rendered_size_cells"
        );
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

    #[test]
    fn test_brightness_simd_and_contrast_lut_preserve_alpha() {
        let mut pixels = vec![
            100, 100, 100, 0, // fully transparent
            100, 100, 100, 128, // semi-transparent
            100, 100, 100, 255, // opaque
        ];

        // 1. Positive and negative brightness
        apply_brightness_simd(&mut pixels, 50);
        assert_eq!(pixels[0..3], [150, 150, 150]);
        assert_eq!(pixels[3], 0);
        assert_eq!(pixels[4..7], [150, 150, 150]);
        assert_eq!(pixels[7], 128);
        assert_eq!(pixels[8..11], [150, 150, 150]);
        assert_eq!(pixels[11], 255);

        apply_brightness_simd(&mut pixels, -100);
        assert_eq!(pixels[0..3], [50, 50, 50]);
        assert_eq!(pixels[3], 0);
        assert_eq!(pixels[4..7], [50, 50, 50]);
        assert_eq!(pixels[7], 128);
        assert_eq!(pixels[8..11], [50, 50, 50]);
        assert_eq!(pixels[11], 255);

        // 2. Contrast LUT
        apply_contrast_lut(&mut pixels, 25.0);
        assert_eq!(pixels[3], 0);
        assert_eq!(pixels[7], 128);
        assert_eq!(pixels[11], 255);
    }

    #[test]
    fn test_soft_knee_highlight_continuity_and_stability() {
        assert_eq!(soft_knee_highlight(0.75), 0.75);

        let eps = 1e-4;
        let slope_left = (soft_knee_highlight(0.75) - soft_knee_highlight(0.75 - eps)) / eps;
        let slope_right = (soft_knee_highlight(0.75 + eps) - soft_knee_highlight(0.75)) / eps;
        assert!((slope_left - 1.0).abs() < 1e-3);
        assert!((slope_right - 1.0).abs() < 1e-3);

        let mut prev = soft_knee_highlight(0.75);
        for i in 1..=50 {
            let x = 0.75 + i as f32 * 0.1;
            let y = soft_knee_highlight(x);
            assert!(y > prev, "Monotonicity violated at x = {}", x);
            assert!(y < 1.0, "Should approach 1.0 asymptotically, got {}", y);
            prev = y;
        }

        let extreme = soft_knee_highlight(1e20);
        assert!(!extreme.is_nan());
        assert!(!extreme.is_infinite());
        assert!((extreme - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_anti_ringing_clamp_eliminates_overshoot() {
        // 4x2 source: columns 0 and 1 black, columns 2 and 3 white
        let mut src = image::RgbaImage::new(4, 2);
        for y in 0..2 {
            src.put_pixel(0, y, image::Rgba([0, 0, 0, 255]));
            src.put_pixel(1, y, image::Rgba([0, 0, 0, 255]));
            src.put_pixel(2, y, image::Rgba([255, 255, 255, 255]));
            src.put_pixel(3, y, image::Rgba([255, 255, 255, 255]));
        }

        // 1. Upscaled target (8x4 >= 4x2) with ringing overshoot at (0, 0)
        let mut dst_up = image::RgbaImage::new(8, 4);
        dst_up.put_pixel(0, 0, image::Rgba([75, 75, 75, 255]));

        clamp_anti_ringing_rgba(&mut dst_up, &src, None);

        // Pixel (0, 0) maps to local texels which are all 0, so overshoot clamped to 0
        assert_eq!(dst_up.get_pixel(0, 0), &image::Rgba([0, 0, 0, 255]));

        // 2. Downscaled target (2x1 < 4x2): should be a no-op to preserve anti-aliased averages
        let mut dst_down = image::RgbaImage::new(2, 1);
        dst_down.put_pixel(0, 0, image::Rgba([75, 75, 75, 255]));

        clamp_anti_ringing_rgba(&mut dst_down, &src, None);

        // Clamping must not modify downscaled images
        assert_eq!(dst_down.get_pixel(0, 0), &image::Rgba([75, 75, 75, 255]));
    }
}
