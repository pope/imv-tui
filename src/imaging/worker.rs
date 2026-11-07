//! CPU-bound image scaling, panning, and resizing worker pools.

use fast_image_resize as fir;
use image::{DynamicImage, GenericImage};
use ratatui_image::protocol::StatefulProtocol;

use crate::imaging::ResizeRequest;
use crate::imaging::types::*;

/// Analytical single-sine Lanczos3 filter kernel.
///
/// Uses the triple-angle trigonometric identity $\sin(\pi x) = \sin(3\theta) = \sin(\theta)(3 - 4\sin^2\theta)$
/// where $\theta = \frac{\pi}{3} x$, evaluating only a single sine call per tap instead of two while
/// remaining mathematically identical to $\text{sinc}(x) \cdot \text{sinc}(x / 3.0)$.
pub fn fast_lanczos3_kernel(x: f64) -> f64 {
    if x.abs() < 1e-8 {
        return 1.0;
    }
    if !(-3.0..3.0).contains(&x) {
        return 0.0;
    }
    let theta = (std::f64::consts::PI / 3.0) * x;
    let s = theta.sin();
    let s2 = s * s;
    (s2 * (3.0 - 4.0 * s2)) / (3.0 * theta * theta)
}

/// Division-free Horner polynomial Mitchell-Netravali ($B = 1/3, C = 1/3$) filter kernel.
///
/// Evaluates the cubic piecewise polynomial using Horner's method with compile-time reciprocal constants.
pub fn fast_mitchell_kernel(mut x: f64) -> f64 {
    x = x.abs();
    if x < 1.0 {
        ((7.0 / 6.0) * x - 2.0) * x * x + (8.0 / 9.0)
    } else if x < 2.0 {
        (((-7.0 / 18.0) * x + 2.0) * x - (10.0 / 3.0)) * x + (16.0 / 9.0)
    } else {
        0.0
    }
}

/// Gaussian ($\sigma = 0.5$) filter kernel with precomputed normalization.
///
/// Evaluates $\frac{1}{\sigma \sqrt{2\pi}} e^{-\frac{x^2}{2\sigma^2}}$ with compile-time $\sqrt{\frac{2}{\pi}}$ constant.
pub fn fast_gaussian_kernel(x: f64) -> f64 {
    if (-3.0..3.0).contains(&x) {
        const INV_SQRT_2PI_HALF: f64 = 0.7978845608028654;
        INV_SQRT_2PI_HALF * (-2.0 * x * x).exp()
    } else {
        0.0
    }
}

/// Maps `FilterType` to `fir::ResizeAlg`, utilizing optimized custom analytical kernels
/// for Mitchell, Gaussian, and Lanczos.
pub fn to_fir_resize_alg(filter_type: FilterType) -> fir::ResizeAlg {
    match filter_type {
        FilterType::Nearest => fir::ResizeAlg::Nearest,
        FilterType::Linear => fir::ResizeAlg::Convolution(fir::FilterType::Bilinear),
        FilterType::Cubic => fir::ResizeAlg::Convolution(fir::FilterType::CatmullRom),
        FilterType::Mitchell => fir::ResizeAlg::Convolution(fir::FilterType::Custom(
            fir::Filter::new("Mitchell", fast_mitchell_kernel, 2.0)
                .expect("Valid custom Mitchell filter"),
        )),
        FilterType::Gaussian => fir::ResizeAlg::Convolution(fir::FilterType::Custom(
            fir::Filter::new("Gaussian", fast_gaussian_kernel, 3.0)
                .expect("Valid custom Gaussian filter"),
        )),
        FilterType::Lanczos => fir::ResizeAlg::Convolution(fir::FilterType::Custom(
            fir::Filter::new("Lanczos3", fast_lanczos3_kernel, 3.0)
                .expect("Valid custom Lanczos3 filter"),
        )),
        FilterType::Hamming => fir::ResizeAlg::Convolution(fir::FilterType::Hamming),
    }
}

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

    let resize_alg = to_fir_resize_alg(filter_type);

    let (crop_w, crop_h) = match crop_rect {
        Some((_, _, w, h)) => (w, h),
        None => (src_img.width() as f64, src_img.height() as f64),
    };
    let is_magnified = dst_w as f64 >= crop_w && dst_h as f64 >= crop_h;
    let needs_anti_ringing = is_magnified
        && matches!(
            filter_type,
            FilterType::Lanczos | FilterType::Mitchell | FilterType::Cubic
        );

    match src_img {
        DynamicImage::ImageRgb8(rgb_src) => {
            let mut dst_image = FirImage::new(dst_w, dst_h, fir::PixelType::U8x3);

            let mut options = fir::ResizeOptions::new();
            options.algorithm = resize_alg;
            options.mul_div_alpha = false;
            if let Some((left, top, width, height)) = crop_rect {
                options = options.crop(left, top, width, height);
            }

            resizer.resize(rgb_src, &mut dst_image, Some(&options))?;

            let rgb_dst = image::RgbImage::from_raw(dst_w, dst_h, dst_image.into_vec())
                .ok_or("Failed to construct RgbImage from resized buffer")?;

            let rgba_raw = crate::imaging::unpack_rgb24_to_rgba32(&rgb_dst);
            let mut rgba_dst = image::RgbaImage::from_raw(dst_w, dst_h, rgba_raw)
                .ok_or("Failed to construct RgbaImage from unpacked buffer")?;

            if needs_anti_ringing {
                clamp_anti_ringing_rgb(&mut rgba_dst, rgb_src, crop_rect);
            }

            Ok(DynamicImage::ImageRgba8(rgba_dst))
        }
        _ => {
            let rgba_src = crate::imaging::to_rgba8_fast(src_img);

            let mut dst_image = FirImage::new(dst_w, dst_h, fir::PixelType::U8x4);

            let mut options = fir::ResizeOptions::new();
            options.algorithm = resize_alg;
            options.mul_div_alpha = true;
            if let Some((left, top, width, height)) = crop_rect {
                options = options.crop(left, top, width, height);
            }

            resizer.resize(rgba_src.as_ref(), &mut dst_image, Some(&options))?;

            let buffer = dst_image.into_vec();
            let mut rgba_dst = image::RgbaImage::from_raw(dst_w, dst_h, buffer)
                .ok_or("Failed to construct RgbaImage from resized buffer")?;

            if needs_anti_ringing {
                clamp_anti_ringing_rgba(&mut rgba_dst, &rgba_src, crop_rect);
            }

            Ok(DynamicImage::ImageRgba8(rgba_dst))
        }
    }
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
    clamp_anti_ringing_span_cached::<4>(
        dst,
        src.as_raw().as_chunks::<4>().0,
        src.width(),
        src.height(),
        crop_rect,
    );
}

/// Clamps destination pixel colors to the min/max bounding envelope of the corresponding
/// 2x2 source texel neighborhood of an RGB source image to eliminate Gibbs ringing/overshoot artifacts.
pub fn clamp_anti_ringing_rgb(
    dst: &mut image::RgbaImage,
    src: &image::RgbImage,
    crop_rect: Option<(f64, f64, f64, f64)>,
) {
    clamp_anti_ringing_span_cached::<3>(
        dst,
        src.as_raw().as_chunks::<3>().0,
        src.width(),
        src.height(),
        crop_rect,
    );
}

fn clamp_anti_ringing_span_cached<const N: usize>(
    dst: &mut image::RgbaImage,
    src_pixels: &[[u8; N]],
    src_w: u32,
    src_h: u32,
    crop_rect: Option<(f64, f64, f64, f64)>,
) {
    let dst_w = dst.width();
    let dst_h = dst.height();

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

    // Pre-tabulate 1D horizontal source bounds for each destination x coordinate
    let mut x_bounds = Vec::with_capacity(dst_w as usize);
    for x in 0..dst_w {
        let src_x_f = crop_left + (x as f64 + 0.5) * scale_x - 0.5;
        let x0 = (src_x_f.floor() as i64).clamp(0, src_w as i64 - 1) as usize;
        let x1 = (x0 + 1).min(src_w as usize - 1);
        x_bounds.push((x0, x1));
    }

    let src_w_usize = src_w as usize;
    let dst_pixels = dst.as_mut().as_chunks_mut::<4>().0;

    // Process rows with horizontal span caching
    for (y, dst_row) in dst_pixels.chunks_exact_mut(dst_w as usize).enumerate() {
        let src_y_f = crop_top + (y as f64 + 0.5) * scale_y - 0.5;
        let y0 = (src_y_f.floor() as i64).clamp(0, src_h as i64 - 1) as usize;
        let y1 = (y0 + 1).min(src_h as usize - 1);

        let row0 = &src_pixels[y0 * src_w_usize..(y0 + 1) * src_w_usize];
        let row1 = &src_pixels[y1 * src_w_usize..(y1 + 1) * src_w_usize];

        let mut prev_x0 = usize::MAX;
        let mut min_rgb = [0u8; 3];
        let mut max_rgb = [0u8; 3];

        for (dst_pixel, &(x0, x1)) in dst_row.iter_mut().zip(&x_bounds) {
            if x0 != prev_x0 {
                prev_x0 = x0;
                let p00 = row0[x0];
                let p10 = row0[x1];
                let p01 = row1[x0];
                let p11 = row1[x1];

                min_rgb[0] = p00[0].min(p10[0]).min(p01[0]).min(p11[0]);
                max_rgb[0] = p00[0].max(p10[0]).max(p01[0]).max(p11[0]);

                min_rgb[1] = p00[1].min(p10[1]).min(p01[1]).min(p11[1]);
                max_rgb[1] = p00[1].max(p10[1]).max(p01[1]).max(p11[1]);

                min_rgb[2] = p00[2].min(p10[2]).min(p01[2]).min(p11[2]);
                max_rgb[2] = p00[2].max(p10[2]).max(p01[2]).max(p11[2]);
            }

            dst_pixel[0] = dst_pixel[0].clamp(min_rgb[0], max_rgb[0]);
            dst_pixel[1] = dst_pixel[1].clamp(min_rgb[1], max_rgb[1]);
            dst_pixel[2] = dst_pixel[2].clamp(min_rgb[2], max_rgb[2]);
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

        // 3. Test clamp_anti_ringing_rgb overshoot elimination
        let mut rgb_overshoot_src = image::RgbImage::new(4, 2);
        for y in 0..2 {
            rgb_overshoot_src.put_pixel(0, y, image::Rgb([0, 0, 0]));
            rgb_overshoot_src.put_pixel(1, y, image::Rgb([0, 0, 0]));
            rgb_overshoot_src.put_pixel(2, y, image::Rgb([255, 255, 255]));
            rgb_overshoot_src.put_pixel(3, y, image::Rgb([255, 255, 255]));
        }
        let mut dst_rgb_clamped = image::RgbaImage::new(8, 4);
        dst_rgb_clamped.put_pixel(0, 0, image::Rgba([75, 75, 75, 255]));
        clamp_anti_ringing_rgb(&mut dst_rgb_clamped, &rgb_overshoot_src, None);
        assert_eq!(
            dst_rgb_clamped.get_pixel(0, 0),
            &image::Rgba([0, 0, 0, 255])
        );

        // 4. Test equivalence between clamp_anti_ringing_rgba and clamp_anti_ringing_rgb
        // across multi-pixel magnified spans and cropped sub-regions.
        let mut src_rgba_span = image::RgbaImage::new(10, 10);
        let mut src_rgb_span = image::RgbImage::new(10, 10);
        for y in 0..10 {
            for x in 0..10 {
                let r = ((x * 25) % 256) as u8;
                let g = ((y * 25) % 256) as u8;
                let b = (((x + y) * 20) % 256) as u8;
                src_rgba_span.put_pixel(x, y, image::Rgba([r, g, b, 255]));
                src_rgb_span.put_pixel(x, y, image::Rgb([r, g, b]));
            }
        }

        // 4a. Full image 5x magnification (50x50 target) with synthetic ringing
        let mut dst_rgba_span = image::RgbaImage::new(50, 50);
        let mut dst_rgb_span = image::RgbaImage::new(50, 50);
        for y in 0..50 {
            for x in 0..50 {
                let val = ((x + y * 2) % 256) as u8;
                dst_rgba_span.put_pixel(x, y, image::Rgba([val, val, val, 255]));
                dst_rgb_span.put_pixel(x, y, image::Rgba([val, val, val, 255]));
            }
        }

        clamp_anti_ringing_rgba(&mut dst_rgba_span, &src_rgba_span, None);
        clamp_anti_ringing_rgb(&mut dst_rgb_span, &src_rgb_span, None);
        assert_eq!(dst_rgba_span, dst_rgb_span);

        // 4b. Cropped sub-region (2.0, 2.0, 4.0, 4.0)
        let crop = Some((2.0, 2.0, 4.0, 4.0));
        let mut dst_rgba_crop = image::RgbaImage::new(40, 40);
        let mut dst_rgb_crop = image::RgbaImage::new(40, 40);
        for y in 0..40 {
            for x in 0..40 {
                let val = ((x * 3 + y) % 256) as u8;
                dst_rgba_crop.put_pixel(x, y, image::Rgba([val, val, val, 255]));
                dst_rgb_crop.put_pixel(x, y, image::Rgba([val, val, val, 255]));
            }
        }

        clamp_anti_ringing_rgba(&mut dst_rgba_crop, &src_rgba_span, crop);
        clamp_anti_ringing_rgb(&mut dst_rgb_crop, &src_rgb_span, crop);
        assert_eq!(dst_rgba_crop, dst_rgb_crop);
    }

    #[test]
    fn test_custom_filter_kernels_mathematical_fidelity() {
        // 1. Lanczos3 reference sinc(x) * sinc(x / 3.0)
        fn sinc(x: f64) -> f64 {
            if x == 0.0 {
                1.0
            } else {
                let px = std::f64::consts::PI * x;
                px.sin() / px
            }
        }
        fn ref_lanczos3(x: f64) -> f64 {
            if (-3.0..3.0).contains(&x) {
                sinc(x) * sinc(x / 3.0)
            } else {
                0.0
            }
        }

        assert_eq!(fast_lanczos3_kernel(0.0), 1.0);
        assert_eq!(fast_lanczos3_kernel(3.0), 0.0);
        assert_eq!(fast_lanczos3_kernel(3.5), 0.0);
        assert_eq!(fast_lanczos3_kernel(-4.2), 0.0);

        // Sample points in (-3.0, 3.0)
        for i in -2999..=2999 {
            let x = i as f64 / 1000.0;
            let expected = ref_lanczos3(x);
            let actual = fast_lanczos3_kernel(x);
            assert!(
                (actual - expected).abs() < 1e-14,
                "Lanczos3 mismatch at x = {}: actual = {}, expected = {}",
                x,
                actual,
                expected
            );
        }

        // Sample points outside [-3.0, 3.0]
        for i in [31, 35, 40, 50, -31, -35, -40, -50] {
            let x = i as f64 / 10.0;
            assert_eq!(fast_lanczos3_kernel(x), 0.0);
            assert_eq!(ref_lanczos3(x), 0.0);
        }

        // 2. Mitchell-Netravali reference formula (B = 1/3, C = 1/3)
        fn ref_mitchell(x: f64) -> f64 {
            let x = x.abs();
            let b = 1.0 / 3.0;
            let c = 1.0 / 3.0;
            if x < 1.0 {
                (1.0 / 6.0)
                    * ((12.0 - 9.0 * b - 6.0 * c) * x.powi(3)
                        + (-18.0 + 12.0 * b + 6.0 * c) * x.powi(2)
                        + (6.0 - 2.0 * b))
            } else if x < 2.0 {
                (1.0 / 6.0)
                    * ((-b - 6.0 * c) * x.powi(3)
                        + (6.0 * b + 30.0 * c) * x.powi(2)
                        + (-12.0 * b - 48.0 * c) * x
                        + (8.0 * b + 24.0 * c))
            } else {
                0.0
            }
        }

        for i in 0..=2500 {
            let x = i as f64 / 1000.0;
            let expected = ref_mitchell(x);
            let actual = fast_mitchell_kernel(x);
            assert!(
                (actual - expected).abs() < 1e-14,
                "Mitchell mismatch at x = {}: actual = {}, expected = {}",
                x,
                actual,
                expected
            );
        }

        // 3. Gaussian reference formula (sigma = 0.5)
        fn ref_gaussian(x: f64) -> f64 {
            if (-3.0..3.0).contains(&x) {
                let sigma = 0.5;
                (1.0 / (sigma * (2.0 * std::f64::consts::PI).sqrt()))
                    * (-0.5 * (x / sigma).powi(2)).exp()
            } else {
                0.0
            }
        }

        for i in -3500..=3500 {
            let x = i as f64 / 1000.0;
            let expected = ref_gaussian(x);
            let actual = fast_gaussian_kernel(x);
            assert!(
                (actual - expected).abs() < 1e-14,
                "Gaussian mismatch at x = {}: actual = {}, expected = {}",
                x,
                actual,
                expected
            );
        }
    }

    #[test]
    fn test_fast_resize_rgb8_direct_and_no_alpha_buffer() {
        let mut resizer = fir::Resizer::new();

        // 1. Create a 100x100 ImageRgb8
        let mut rgb_raw = image::RgbImage::new(100, 100);
        for (x, y, pixel) in rgb_raw.enumerate_pixels_mut() {
            *pixel = image::Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8]);
        }
        let src_rgb = DynamicImage::ImageRgb8(rgb_raw);

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
            let downscaled = fast_resize(&mut resizer, &src_rgb, 40, 30, filter, None)
                .expect("Downscale failed");
            assert_eq!(downscaled.width(), 40);
            assert_eq!(downscaled.height(), 30);
            let down_rgba = downscaled.to_rgba8();
            assert_eq!(down_rgba.get_pixel(0, 0)[3], 255);
            assert_eq!(down_rgba.get_pixel(39, 29)[3], 255);

            // Upscale (triggers clamp_anti_ringing_rgb on lobed filters)
            let upscaled = fast_resize(&mut resizer, &src_rgb, 200, 150, filter, None)
                .expect("Upscale failed");
            assert_eq!(upscaled.width(), 200);
            assert_eq!(upscaled.height(), 150);
            let up_rgba = upscaled.to_rgba8();
            assert_eq!(up_rgba.get_pixel(0, 0)[3], 255);
            assert_eq!(up_rgba.get_pixel(199, 149)[3], 255);

            // Cropped resize
            let crop = Some((10.0, 10.0, 50.0, 50.0));
            let cropped = fast_resize(&mut resizer, &src_rgb, 80, 80, filter, crop)
                .expect("Cropped resize failed");
            assert_eq!(cropped.width(), 80);
            assert_eq!(cropped.height(), 80);
            let crop_rgba = cropped.to_rgba8();
            assert_eq!(crop_rgba.get_pixel(0, 0)[3], 255);
            assert_eq!(crop_rgba.get_pixel(79, 79)[3], 255);
        }

        // 2. Verify that resizing ImageRgb8 does NOT allocate an alpha_buffer in resizer
        // For a 500x500 image, an alpha_buffer would allocate at least 500 * 500 * 4 = 1,000,000 bytes.
        let large_rgb = DynamicImage::ImageRgb8(image::RgbImage::new(500, 500));
        let large_rgba = DynamicImage::ImageRgba8(image::RgbaImage::new(500, 500));

        resizer.reset_internal_buffers();
        assert_eq!(resizer.size_of_internal_buffers(), 0);

        fast_resize(&mut resizer, &large_rgb, 50, 50, FilterType::Mitchell, None).unwrap();
        let rgb_internal_buf_size = resizer.size_of_internal_buffers();

        resizer.reset_internal_buffers();
        fast_resize(
            &mut resizer,
            &large_rgba,
            50,
            50,
            FilterType::Mitchell,
            None,
        )
        .unwrap();
        let rgba_internal_buf_size = resizer.size_of_internal_buffers();

        // RGBA allocates 1 MB alpha_buffer, whereas RGB8 avoids alpha_buffer entirely
        assert!(rgba_internal_buf_size >= 1_000_000);
        assert!(rgb_internal_buf_size < 150_000);
        assert!(rgba_internal_buf_size > rgb_internal_buf_size + 800_000);

        // For Nearest, RGB8 requires 0 bytes of internal buffers
        resizer.reset_internal_buffers();
        fast_resize(&mut resizer, &large_rgb, 50, 50, FilterType::Nearest, None).unwrap();
        assert_eq!(resizer.size_of_internal_buffers(), 0);
    }
}
