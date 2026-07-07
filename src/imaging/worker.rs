//! CPU-bound image scaling, panning, and resizing worker pools.

use fast_image_resize as fir;
use image::{DynamicImage, GenericImage};
use ratatui_image::protocol::StatefulProtocol;
use std::sync::Arc;

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

/// Processes a scaling and panning request in the background, creating/rendering
/// the final scaled viewport on a screen-pixel canvas block to support offscreen panning boundaries.
pub fn process_resize(
    req: ResizeRequest,
    resizer: &mut fir::Resizer,
) -> (StatefulProtocol, std::time::Duration, std::time::Duration) {
    let start_process = std::time::Instant::now();

    // Map input crop/intersection coordinates from the full image space to the thumbnail space
    // if the loaded image buffer is a thumbnail placeholder of different dimensions.
    let (img_to_resize, scale, crop, intersection) =
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

            (Arc::clone(&req.img), new_scale, scaled_crop, scaled_inter)
        } else {
            (Arc::clone(&req.img), req.scale, req.crop, req.intersection)
        };

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
            &img_to_resize,
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
                &img_to_resize,
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
    let process_duration = start_process.elapsed();

    let start_protocol = std::time::Instant::now();
    let protocol = req.picker.new_resize_protocol(canvas);
    let protocol_duration = start_protocol.elapsed();

    (protocol, process_duration, protocol_duration)
}
