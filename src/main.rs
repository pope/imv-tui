use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use fast_image_resize as fir;
use image::{DynamicImage, GenericImage, ImageDecoder};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterType {
    Nearest,
    Triangle,
    CatmullRom,
    Mitchell,
    Gaussian,
    Lanczos3,
    Hamming,
}

impl FilterType {
    pub fn to_image_filter(self) -> image::imageops::FilterType {
        match self {
            FilterType::Nearest => image::imageops::FilterType::Nearest,
            FilterType::Triangle => image::imageops::FilterType::Triangle,
            FilterType::CatmullRom => image::imageops::FilterType::CatmullRom,
            FilterType::Mitchell => image::imageops::FilterType::CatmullRom,
            FilterType::Gaussian => image::imageops::FilterType::Gaussian,
            FilterType::Lanczos3 => image::imageops::FilterType::Lanczos3,
            FilterType::Hamming => image::imageops::FilterType::Triangle,
        }
    }
}
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Style, Stylize},
    text::Line,
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use ratatui_image::{
    StatefulImage,
    picker::{Picker, ProtocolType},
    protocol::StatefulProtocol,
};

fn decode_image_source(
    source: ImageSource,
) -> Result<(DynamicImage, u32, u32, &'static str), String> {
    match source {
        ImageSource::Local(path) => {
            let format = image::ImageReader::open(&path)
                .and_then(|r| r.with_guessed_format())
                .map(|r| r.format());

            if let Ok(Some(image::ImageFormat::Jpeg)) = format
                && let Ok(bytes) = std::fs::read(&path)
            {
                let options = zune_jpeg::zune_core::options::DecoderOptions::default()
                    .jpeg_set_out_colorspace(zune_jpeg::zune_core::colorspace::ColorSpace::RGBA);
                let mut decoder = zune_jpeg::JpegDecoder::new_with_options(&bytes, options);
                if let Ok(pixels) = decoder.decode()
                    && let Some(info) = decoder.info()
                    && let Some(rgba_img) =
                        image::RgbaImage::from_raw(info.width as u32, info.height as u32, pixels)
                {
                    let orientation = match image::ImageReader::open(&path)
                        .and_then(|r| r.with_guessed_format())
                    {
                        Ok(reader) => match reader.into_decoder() {
                            Ok(mut dec) => dec
                                .orientation()
                                .unwrap_or(image::metadata::Orientation::NoTransforms),
                            Err(_) => image::metadata::Orientation::NoTransforms,
                        },
                        Err(_) => image::metadata::Orientation::NoTransforms,
                    };

                    let mut img = image::DynamicImage::ImageRgba8(rgba_img);
                    img.apply_orientation(orientation);
                    let w = img.width();
                    let h = img.height();
                    return Ok((img, w, h, "\u{F0225}"));
                }
            }

            let reader = image::ImageReader::open(&path)
                .map_err(|e| format!("Failed to open file:\n{}\n\nError: {}", path.display(), e))?
                .with_guessed_format()
                .map_err(|e| {
                    format!(
                        "Failed to guess format for:\n{}\n\nError: {}",
                        path.display(),
                        e
                    )
                })?;

            let fmt = reader.format();
            let icon = match fmt {
                Some(image::ImageFormat::Jpeg) => "\u{F0225}",
                Some(image::ImageFormat::Png) => "\u{F0E2D}",
                Some(image::ImageFormat::Gif) => "\u{F0D78}",
                _ => "\u{F021F}",
            };

            let mut decoder = reader.into_decoder().map_err(|e| {
                format!(
                    "Failed to read metadata:\n{}\n\nError: {}",
                    path.display(),
                    e
                )
            })?;

            let orientation = decoder
                .orientation()
                .unwrap_or(image::metadata::Orientation::NoTransforms);
            let mut img = image::DynamicImage::from_decoder(decoder).map_err(|e| {
                format!(
                    "Failed to decode image:\n{}\n\nError: {}",
                    path.display(),
                    e
                )
            })?;

            img.apply_orientation(orientation);
            let rgba_img = img.to_rgba8();
            let w = rgba_img.width();
            let h = rgba_img.height();
            Ok((image::DynamicImage::ImageRgba8(rgba_img), w, h, icon))
        }
        ImageSource::Cbz {
            zip_path,
            file_in_zip,
        } => {
            let file = std::fs::File::open(&zip_path)
                .map_err(|e| format!("Failed to open zip file {}: {}", zip_path.display(), e))?;
            let reader = std::io::BufReader::new(file);
            let mut archive = zip::ZipArchive::new(reader)
                .map_err(|e| format!("Failed to read zip archive {}: {}", zip_path.display(), e))?;
            let mut zip_entry = archive.by_name(&file_in_zip).map_err(|e| {
                format!(
                    "Failed to locate page {} in {}: {}",
                    file_in_zip,
                    zip_path.display(),
                    e
                )
            })?;
            let mut buffer = Vec::with_capacity(zip_entry.size() as usize);
            use std::io::Read;
            zip_entry.read_to_end(&mut buffer).map_err(|e| {
                format!(
                    "Failed to read page data {} from {}: {}",
                    file_in_zip,
                    zip_path.display(),
                    e
                )
            })?;

            let cursor = std::io::Cursor::new(buffer.clone());
            let reader = image::ImageReader::new(cursor)
                .with_guessed_format()
                .map_err(|e| format!("Failed to guess image format for {}: {}", file_in_zip, e))?;

            if let Some(image::ImageFormat::Jpeg) = reader.format() {
                let options = zune_jpeg::zune_core::options::DecoderOptions::default()
                    .jpeg_set_out_colorspace(zune_jpeg::zune_core::colorspace::ColorSpace::RGBA);
                let mut decoder = zune_jpeg::JpegDecoder::new_with_options(&buffer, options);
                if let Ok(pixels) = decoder.decode()
                    && let Some(info) = decoder.info()
                    && let Some(rgba_img) =
                        image::RgbaImage::from_raw(info.width as u32, info.height as u32, pixels)
                {
                    let cursor_meta = std::io::Cursor::new(buffer);
                    let orientation =
                        match image::ImageReader::new(cursor_meta).with_guessed_format() {
                            Ok(reader) => match reader.into_decoder() {
                                Ok(mut dec) => dec
                                    .orientation()
                                    .unwrap_or(image::metadata::Orientation::NoTransforms),
                                Err(_) => image::metadata::Orientation::NoTransforms,
                            },
                            Err(_) => image::metadata::Orientation::NoTransforms,
                        };

                    let mut img = image::DynamicImage::ImageRgba8(rgba_img);
                    img.apply_orientation(orientation);
                    let w = img.width();
                    let h = img.height();
                    return Ok((img, w, h, "\u{F0225}"));
                }
            }

            let cursor = std::io::Cursor::new(buffer);
            let reader = image::ImageReader::new(cursor)
                .with_guessed_format()
                .map_err(|e| format!("Failed to guess image format for {}: {}", file_in_zip, e))?;

            let fmt = reader.format();
            let icon = match fmt {
                Some(image::ImageFormat::Jpeg) => "\u{F0225}",
                Some(image::ImageFormat::Png) => "\u{F0E2D}",
                Some(image::ImageFormat::Gif) => "\u{F0D78}",
                _ => "\u{F021F}",
            };

            let mut decoder = reader
                .into_decoder()
                .map_err(|e| format!("Failed to decode header for {}: {}", file_in_zip, e))?;
            let orientation = decoder
                .orientation()
                .unwrap_or(image::metadata::Orientation::NoTransforms);
            let mut img = image::DynamicImage::from_decoder(decoder)
                .map_err(|e| format!("Failed to decode image data for {}: {}", file_in_zip, e))?;
            img.apply_orientation(orientation);

            let rgba_img = img.to_rgba8();
            let w = rgba_img.width();
            let h = rgba_img.height();
            Ok((image::DynamicImage::ImageRgba8(rgba_img), w, h, icon))
        }
    }
}

struct ResizeRequest {
    img: Arc<DynamicImage>,
    scale: f64,
    crop_x1: i64,
    crop_y1: i64,
    crop_x2: i64,
    crop_y2: i64,
    inter_x1: i64,
    inter_y1: i64,
    inter_x2: i64,
    inter_y2: i64,
    target_w: u32,
    target_h: u32,
    filter_type: FilterType,
    picker: Picker,
    brightness: i32,
    contrast: f32,
    rendered_size_cells: (u16, u16),
}

fn fast_resize(
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
        FilterType::Triangle => fir::ResizeAlg::Convolution(fir::FilterType::Bilinear),
        FilterType::CatmullRom => fir::ResizeAlg::Convolution(fir::FilterType::CatmullRom),
        FilterType::Mitchell => fir::ResizeAlg::Convolution(fir::FilterType::Mitchell),
        FilterType::Gaussian => fir::ResizeAlg::Convolution(fir::FilterType::Gaussian),
        FilterType::Lanczos3 => fir::ResizeAlg::Convolution(fir::FilterType::Lanczos3),
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

fn process_resize(req: ResizeRequest, resizer: &mut fir::Resizer) -> StatefulProtocol {
    let mut canvas = if req.inter_x1 == req.crop_x1
        && req.inter_x2 == req.crop_x2
        && req.inter_y1 == req.crop_y1
        && req.inter_y2 == req.crop_y2
    {
        let crop_rect = Some((
            req.inter_x1 as f64,
            req.inter_y1 as f64,
            (req.inter_x2 - req.inter_x1) as f64,
            (req.inter_y2 - req.inter_y1) as f64,
        ));
        match fast_resize(
            resizer,
            &req.img,
            req.target_w,
            req.target_h,
            req.filter_type,
            crop_rect,
        ) {
            Ok(resized) => resized,
            Err(_) => {
                let cropped_part = req.img.crop_imm(
                    req.inter_x1 as u32,
                    req.inter_y1 as u32,
                    (req.inter_x2 - req.inter_x1) as u32,
                    (req.inter_y2 - req.inter_y1) as u32,
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

        if req.inter_x2 > req.inter_x1 && req.inter_y2 > req.inter_y1 {
            let target_inter_w =
                (((req.inter_x2 - req.inter_x1) as f64 * req.scale).round() as u32).max(1);
            let target_inter_h =
                (((req.inter_y2 - req.inter_y1) as f64 * req.scale).round() as u32).max(1);

            let crop_rect = Some((
                req.inter_x1 as f64,
                req.inter_y1 as f64,
                (req.inter_x2 - req.inter_x1) as f64,
                (req.inter_y2 - req.inter_y1) as f64,
            ));

            let resized_part = match fast_resize(
                resizer,
                &req.img,
                target_inter_w,
                target_inter_h,
                req.filter_type,
                crop_rect,
            ) {
                Ok(resized) => resized,
                Err(_) => {
                    let cropped_part = req.img.crop_imm(
                        req.inter_x1 as u32,
                        req.inter_y1 as u32,
                        (req.inter_x2 - req.inter_x1) as u32,
                        (req.inter_y2 - req.inter_y1) as u32,
                    );
                    cropped_part.resize(
                        target_inter_w,
                        target_inter_h,
                        req.filter_type.to_image_filter(),
                    )
                }
            };

            let paste_x = ((req.inter_x1 - req.crop_x1) as f64 * req.scale).round() as i64;
            let paste_y = ((req.inter_y1 - req.crop_y1) as f64 * req.scale).round() as i64;

            let paste_x =
                paste_x.clamp(0, (req.target_w as i64 - target_inter_w as i64).max(0)) as u32;
            let paste_y =
                paste_y.clamp(0, (req.target_h as i64 - target_inter_h as i64).max(0)) as u32;

            if let Some(rgba_part) = resized_part.as_rgba8() {
                let _ = screen_canvas.copy_from(rgba_part, paste_x, paste_y);
            } else {
                let _ = screen_canvas.copy_from(&resized_part.to_rgba8(), paste_x, paste_y);
            }
        }
        DynamicImage::ImageRgba8(screen_canvas)
    };

    if req.brightness != 0 {
        canvas = canvas.brighten(req.brightness);
    }
    if req.contrast != 0.0 {
        canvas = canvas.adjust_contrast(req.contrast);
    }

    req.picker.new_resize_protocol(canvas)
}

struct LoaderRequest {
    idx: usize,
    source: ImageSource,
    is_prefetch: bool,
    sequence: u64,
}

struct LoaderResponse {
    idx: usize,
    result: Result<(DynamicImage, u32, u32, &'static str), String>,
    is_prefetch: bool,
    sequence: u64,
}

#[derive(Clone, Debug)]
pub enum ImageSource {
    Local(PathBuf),
    Cbz {
        zip_path: PathBuf,
        file_in_zip: String,
    },
}

impl ImageSource {
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
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PromptType {
    GoToImage,
    SetBrightness,
    SetContrast,
    SetSlideshow,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PaletteMode {
    Closed,
    Command,
    File,
    Prompt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleMode {
    None,
    Shrink,
    Full,
    Crop,
}

impl ScaleMode {
    pub fn name(&self) -> &'static str {
        match self {
            ScaleMode::None => "None",
            ScaleMode::Shrink => "Shrink",
            ScaleMode::Full => "Full",
            ScaleMode::Crop => "Crop",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::EnumIter)]
pub enum Command {
    Quit,
    NextImage,
    PreviousImage,
    ZoomIn,
    ZoomOut,
    PredefinedZoomIn,
    PredefinedZoomOut,
    ActualSize,
    ResetView,
    IncreaseBrightness,
    IncreaseContrast,
    PanLeft,
    PanRight,
    RotateClockwise,
    RotateCounterClockwise,
    NextFilter,
    CycleScaleMode,
    CommandPalette,
    FileSearch,
    ShowHelp,

    // Non-help/derived commands:
    SetFilterNearest,
    SetFilterLinear,
    SetFilterCubic,
    SetFilterMitchell,
    SetFilterGaussian,
    SetFilterLanczos,
    SetFilterHamming,
    GoToImage,
    SetBrightness,
    SetContrast,
    SetScaleNone,
    SetScaleShrink,
    SetScaleFit,
    SetScaleCrop,
    DecreaseBrightness,
    DecreaseContrast,
    PanUp,
    PanDown,
    ToggleHelp,
    SlideshowIncrease,
    SlideshowDecrease,
    SetSlideshow,
}

impl Command {
    pub fn from_key(key: event::KeyEvent) -> Option<Self> {
        for cmd in <Self as strum::IntoEnumIterator>::iter() {
            for def in cmd.get_metadata() {
                let bindings = match def.group {
                    CommandGroup::Hidden => &[],
                    CommandGroup::Normal(b) => b,
                    CommandGroup::Brightness(b) => b,
                    CommandGroup::Contrast(b) => b,
                    CommandGroup::Pan(b) => b,
                    CommandGroup::Slideshow(b) => b,
                };
                for bind in bindings {
                    if bind.matches(key) {
                        return Some(cmd);
                    }
                }
            }
        }
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyDef {
    Char(char),
    Code(event::KeyCode),
    Ctrl(char),
    Shift(event::KeyCode),
    MouseScroll,
}

impl KeyDef {
    pub fn matches(self, event: event::KeyEvent) -> bool {
        use event::{KeyCode, KeyModifiers};
        match self {
            Self::Char(c) => {
                if let KeyCode::Char(key_char) = event.code {
                    if event.modifiers.contains(KeyModifiers::CONTROL) {
                        return false;
                    }
                    if c.is_alphabetic() {
                        if c.is_lowercase() {
                            key_char == c && !event.modifiers.contains(KeyModifiers::SHIFT)
                        } else {
                            key_char == c
                                || (event.modifiers.contains(KeyModifiers::SHIFT)
                                    && key_char.to_lowercase().next() == c.to_lowercase().next())
                        }
                    } else {
                        key_char == c
                    }
                } else {
                    false
                }
            }
            Self::Code(code) => {
                event.code == code && !event.modifiers.contains(KeyModifiers::CONTROL)
            }
            Self::Ctrl(c) => {
                if let KeyCode::Char(key_char) = event.code {
                    key_char == c && event.modifiers.contains(KeyModifiers::CONTROL)
                } else {
                    false
                }
            }
            Self::Shift(code) => {
                event.code == code && event.modifiers.contains(KeyModifiers::SHIFT)
            }
            Self::MouseScroll => false,
        }
    }

    pub fn format(self) -> &'static str {
        match self {
            Self::Char(c) => match c {
                ' ' => "Space",
                'q' => "q",
                'n' => "n",
                'p' => "p",
                'i' => "i",
                'o' => "o",
                'a' => "a",
                'r' => "r",
                'b' => "b",
                'B' => "B",
                'c' => "c",
                'C' => "C",
                'e' => "e",
                'E' => "E",
                'R' => "R",
                'S' => "S",
                's' => "s",
                'I' => "I",
                'O' => "O",
                'f' => "f",
                ':' => ":",
                '?' => "?",
                '/' => "/",
                '[' => "[",
                ']' => "]",
                '+' => "+",
                '=' => "=",
                '-' => "-",
                '<' => "<",
                '>' => ">",
                'h' => "h",
                'j' => "j",
                'k' => "k",
                'l' => "l",
                _ => "?",
            },
            Self::Code(code) => match code {
                event::KeyCode::Esc => "Esc",
                event::KeyCode::Backspace => "Backspace",
                event::KeyCode::Left => "Left",
                event::KeyCode::Right => "Right",
                event::KeyCode::Up => "Up",
                event::KeyCode::Down => "Down",
                event::KeyCode::Char(' ') => "Space",
                _ => "Other",
            },
            Self::Ctrl(c) => match c {
                'k' => "Ctrl+k",
                'j' => "Ctrl+j",
                _ => "Ctrl+?",
            },
            Self::Shift(_) => "Shift+?",
            Self::MouseScroll => "Mouse Scroll",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandGroup {
    Hidden,
    Normal(&'static [KeyDef]),
    Brightness(&'static [KeyDef]),
    Contrast(&'static [KeyDef]),
    Pan(&'static [KeyDef]),
    Slideshow(&'static [KeyDef]),
}

#[derive(Debug, Clone, Copy)]
pub struct CommandItem {
    pub name: &'static str,
    pub description: &'static str,
    pub show_in_palette: bool,
    pub group: CommandGroup,
}

#[derive(Debug, Clone, Copy)]
pub struct PaletteCommand {
    pub cmd: Command,
    pub item: &'static CommandItem,
}

impl Command {
    pub fn get_metadata(self) -> &'static [CommandItem] {
        match self {
            Self::ShowHelp => &[CommandItem {
                name: "Show Help",
                description: "Toggle Help",
                show_in_palette: true,
                group: CommandGroup::Normal(&[KeyDef::Char('?'), KeyDef::Char('/')]),
            }],
            Self::ResetView => &[CommandItem {
                name: "Reset View",
                description: "Reset View",
                show_in_palette: true,
                group: CommandGroup::Normal(&[KeyDef::Char('r')]),
            }],
            Self::ActualSize => &[CommandItem {
                name: "Actual Size",
                description: "Actual Size",
                show_in_palette: true,
                group: CommandGroup::Normal(&[KeyDef::Char('a')]),
            }],
            Self::RotateClockwise => &[CommandItem {
                name: "Rotate Clockwise",
                description: "Rotate CW 90°",
                show_in_palette: true,
                group: CommandGroup::Normal(&[
                    KeyDef::Char('e'),
                    KeyDef::Char('R'),
                    KeyDef::Char('>'),
                ]),
            }],
            Self::RotateCounterClockwise => &[CommandItem {
                name: "Rotate Counter-Clockwise",
                description: "Rotate CCW 90°",
                show_in_palette: true,
                group: CommandGroup::Normal(&[KeyDef::Char('E'), KeyDef::Char('<')]),
            }],
            Self::NextImage => &[CommandItem {
                name: "Next Image",
                description: "Next image",
                show_in_palette: true,
                group: CommandGroup::Normal(&[
                    KeyDef::Char('n'),
                    KeyDef::Char(' '),
                    KeyDef::Char(']'),
                ]),
            }],
            Self::PreviousImage => &[CommandItem {
                name: "Previous Image",
                description: "Previous image",
                show_in_palette: true,
                group: CommandGroup::Normal(&[
                    KeyDef::Char('p'),
                    KeyDef::Code(event::KeyCode::Backspace),
                    KeyDef::Char('['),
                ]),
            }],
            Self::ZoomIn => &[
                CommandItem {
                    name: "Zoom In",
                    description: "Zoom In",
                    show_in_palette: true,
                    group: CommandGroup::Normal(&[
                        KeyDef::Char('i'),
                        KeyDef::Char('+'),
                        KeyDef::Char('='),
                    ]),
                },
                CommandItem {
                    name: "Zoom (mouse)",
                    description: "Zoom In / Out",
                    show_in_palette: false,
                    group: CommandGroup::Normal(&[KeyDef::MouseScroll]),
                },
            ],
            Self::ZoomOut => &[CommandItem {
                name: "Zoom Out",
                description: "Zoom Out",
                show_in_palette: true,
                group: CommandGroup::Normal(&[KeyDef::Char('o'), KeyDef::Char('-')]),
            }],
            Self::Quit => &[CommandItem {
                name: "Quit",
                description: "Quit",
                show_in_palette: true,
                group: CommandGroup::Normal(&[
                    KeyDef::Char('q'),
                    KeyDef::Code(event::KeyCode::Esc),
                ]),
            }],
            Self::SetFilterNearest => &[CommandItem {
                name: "Set Filter: Nearest",
                description: "Use Nearest Neighbor scaling (sharp, pixelated)",
                show_in_palette: true,
                group: CommandGroup::Hidden,
            }],
            Self::SetFilterLinear => &[CommandItem {
                name: "Set Filter: Linear",
                description: "Use Bilinear scaling",
                show_in_palette: true,
                group: CommandGroup::Hidden,
            }],
            Self::SetFilterCubic => &[CommandItem {
                name: "Set Filter: Cubic",
                description: "Use Bicubic scaling (Catmull-Rom)",
                show_in_palette: true,
                group: CommandGroup::Hidden,
            }],
            Self::SetFilterMitchell => &[CommandItem {
                name: "Set Filter: Mitchell",
                description: "Use Mitchell-Netravali scaling",
                show_in_palette: true,
                group: CommandGroup::Hidden,
            }],
            Self::SetFilterGaussian => &[CommandItem {
                name: "Set Filter: Gaussian",
                description: "Use Gaussian scaling",
                show_in_palette: true,
                group: CommandGroup::Hidden,
            }],
            Self::SetFilterLanczos => &[CommandItem {
                name: "Set Filter: Lanczos",
                description: "Use Lanczos3 scaling (high quality)",
                show_in_palette: true,
                group: CommandGroup::Hidden,
            }],
            Self::SetFilterHamming => &[CommandItem {
                name: "Set Filter: Hamming",
                description: "Use Hamming scaling",
                show_in_palette: true,
                group: CommandGroup::Hidden,
            }],
            Self::NextFilter => &[CommandItem {
                name: "Next Filter",
                description: "Next scaling filter",
                show_in_palette: true,
                group: CommandGroup::Normal(&[KeyDef::Char('S')]),
            }],
            Self::GoToImage => &[CommandItem {
                name: "Go to Image",
                description: "Jump to a specific image index",
                show_in_palette: true,
                group: CommandGroup::Hidden,
            }],
            Self::SetBrightness => &[CommandItem {
                name: "Set Brightness",
                description: "Set image brightness to an absolute value or offset (e.g. 50, +10, -10)",
                show_in_palette: true,
                group: CommandGroup::Hidden,
            }],
            Self::SetContrast => &[CommandItem {
                name: "Set Contrast",
                description: "Set image contrast percentage to an absolute value or offset (e.g. 20, +5, -5)",
                show_in_palette: true,
                group: CommandGroup::Hidden,
            }],
            Self::SetScaleNone => &[CommandItem {
                name: "Set Scale: None",
                description: "Do not scale the image (show at actual size 1:1)",
                show_in_palette: true,
                group: CommandGroup::Hidden,
            }],
            Self::SetScaleShrink => &[CommandItem {
                name: "Set Scale: Shrink to Fit",
                description: "Scale larger images down to fit, leave smaller images untouched",
                show_in_palette: true,
                group: CommandGroup::Hidden,
            }],
            Self::SetScaleFit => &[CommandItem {
                name: "Set Scale: Fit View",
                description: "Scale images up or down to fit the viewport perfectly",
                show_in_palette: true,
                group: CommandGroup::Hidden,
            }],
            Self::SetScaleCrop => &[CommandItem {
                name: "Set Scale: Crop to Fill",
                description: "Scale images to completely fill the viewport (cropping excess)",
                show_in_palette: true,
                group: CommandGroup::Hidden,
            }],
            Self::CycleScaleMode => &[CommandItem {
                name: "Cycle Scale Mode",
                description: "Cycle scale mode",
                show_in_palette: true,
                group: CommandGroup::Normal(&[KeyDef::Char('s')]),
            }],
            Self::PredefinedZoomIn => &[CommandItem {
                name: "Predefined Zoom In",
                description: "Predefined Zoom In",
                show_in_palette: true,
                group: CommandGroup::Normal(&[KeyDef::Char('I')]),
            }],
            Self::PredefinedZoomOut => &[CommandItem {
                name: "Predefined Zoom Out",
                description: "Predefined Zoom Out",
                show_in_palette: true,
                group: CommandGroup::Normal(&[KeyDef::Char('O')]),
            }],
            Self::IncreaseBrightness => &[CommandItem {
                name: "Increase Brightness",
                description: "Increase brightness by 10",
                show_in_palette: true,
                group: CommandGroup::Brightness(&[KeyDef::Char('b')]),
            }],
            Self::DecreaseBrightness => &[CommandItem {
                name: "Decrease Brightness",
                description: "Decrease brightness by 10",
                show_in_palette: true,
                group: CommandGroup::Brightness(&[KeyDef::Char('B')]),
            }],
            Self::IncreaseContrast => &[CommandItem {
                name: "Increase Contrast",
                description: "Increase contrast by 5%",
                show_in_palette: true,
                group: CommandGroup::Contrast(&[KeyDef::Char('c')]),
            }],
            Self::DecreaseContrast => &[CommandItem {
                name: "Decrease Contrast",
                description: "Decrease contrast by 5%",
                show_in_palette: true,
                group: CommandGroup::Contrast(&[KeyDef::Char('C')]),
            }],
            Self::PanLeft => &[
                CommandItem {
                    name: "Pan Left",
                    description: "Pan view left",
                    show_in_palette: true,
                    group: CommandGroup::Pan(&[KeyDef::Char('h')]),
                },
                CommandItem {
                    name: "Pan Left",
                    description: "Pan view left",
                    show_in_palette: false,
                    group: CommandGroup::Pan(&[KeyDef::Code(event::KeyCode::Left)]),
                },
            ],
            Self::PanRight => &[
                CommandItem {
                    name: "Pan Right",
                    description: "Pan view right",
                    show_in_palette: true,
                    group: CommandGroup::Pan(&[KeyDef::Char('l')]),
                },
                CommandItem {
                    name: "Pan Right",
                    description: "Pan view right",
                    show_in_palette: false,
                    group: CommandGroup::Pan(&[KeyDef::Code(event::KeyCode::Right)]),
                },
            ],
            Self::PanUp => &[
                CommandItem {
                    name: "Pan Up",
                    description: "Pan view up",
                    show_in_palette: true,
                    group: CommandGroup::Pan(&[KeyDef::Char('k')]),
                },
                CommandItem {
                    name: "Pan Up",
                    description: "Pan view up",
                    show_in_palette: false,
                    group: CommandGroup::Pan(&[KeyDef::Code(event::KeyCode::Up)]),
                },
            ],
            Self::PanDown => &[
                CommandItem {
                    name: "Pan Down",
                    description: "Pan view down",
                    show_in_palette: true,
                    group: CommandGroup::Pan(&[KeyDef::Char('j')]),
                },
                CommandItem {
                    name: "Pan Down",
                    description: "Pan view down",
                    show_in_palette: false,
                    group: CommandGroup::Pan(&[KeyDef::Code(event::KeyCode::Down)]),
                },
            ],
            Self::ToggleHelp => &[],
            Self::CommandPalette => &[CommandItem {
                name: "Command Palette",
                description: "Command Palette",
                show_in_palette: true,
                group: CommandGroup::Normal(&[KeyDef::Char(':')]),
            }],
            Self::FileSearch => &[CommandItem {
                name: "File Search",
                description: "File Search",
                show_in_palette: true,
                group: CommandGroup::Normal(&[KeyDef::Char('f')]),
            }],
            Self::SlideshowIncrease => &[CommandItem {
                name: "Increase Slideshow",
                description: "Increase slideshow by 1s",
                show_in_palette: true,
                group: CommandGroup::Slideshow(&[KeyDef::Char('t')]),
            }],
            Self::SlideshowDecrease => &[CommandItem {
                name: "Decrease Slideshow",
                description: "Decrease slideshow by 1s",
                show_in_palette: true,
                group: CommandGroup::Slideshow(&[KeyDef::Char('T')]),
            }],
            Self::SetSlideshow => &[CommandItem {
                name: "Set Slideshow",
                description: "Set slideshow duration in seconds or offset (e.g. 5, +1, -1)",
                show_in_palette: true,
                group: CommandGroup::Hidden,
            }],
        }
    }
}

pub fn get_commands() -> &'static [CommandItem] {
    static LIST: std::sync::OnceLock<Vec<CommandItem>> = std::sync::OnceLock::new();
    LIST.get_or_init(|| {
        let mut list = Vec::new();
        for cmd in <Command as strum::IntoEnumIterator>::iter() {
            list.extend_from_slice(cmd.get_metadata());
        }
        list
    })
}

type PrefetchCache = Arc<Mutex<HashMap<usize, (Arc<DynamicImage>, u32, u32, &'static str)>>>;

/// App state
pub struct App {
    pub images: Vec<ImageSource>,
    pub display_names: Vec<String>,
    pub display_names_lowercase: Vec<String>,
    pub current_index: usize,
    pub original_image: Option<Arc<DynamicImage>>,
    pub image_protocol: Option<StatefulProtocol>,
    pub picker: Picker,

    pub img_width: u32,
    pub img_height: u32,

    pub zoom_factor: f64,
    pub pan_offset: (i64, i64),

    pub running: bool,
    pub show_help: bool,
    pub error_message: Option<String>,

    pub last_widget_size: (u16, u16),
    pub needs_update: bool,
    /// Triggers a visual clearing of the terminal screen, but only if the active protocol requires it (e.g. Sixel).
    /// Used to clean Sixel cells on discrete updates (like image loads) without causing constant Kitty/Halfblocks flicker.
    pub needs_clear: bool,
    /// Triggers an unconditional visual clearing of the terminal text grid on the next frame.
    /// Primarily used to cleanly erase text characters of dismissed dialogues (Help, search palettes) from the image region
    /// in terminals using the Kitty graphics protocol (like WezTerm), where double buffering would otherwise skip/freeze them.
    pub needs_clear_once: bool,
    pub rendered_size_cells: (u16, u16),
    pub current_zoom_pct: f64,
    pub palette_mode: PaletteMode,
    pub palette_query: String,
    pub palette_selected_index: usize,
    pub palette_width: u16,
    pub palette_height: u16,
    pub prompt_type: Option<PromptType>,
    pub filter_type: FilterType,
    pub scale_mode: ScaleMode,
    pub matcher: nucleo::Matcher,
    pub current_icon: &'static str,

    // Thread communication channels
    resize_tx: mpsc::Sender<ResizeRequest>,
    protocol_rx: mpsc::Receiver<(StatefulProtocol, (u16, u16))>,
    loader_tx: mpsc::Sender<LoaderRequest>,
    response_rx: mpsc::Receiver<LoaderResponse>,
    current_sequence: u64,
    pub is_loading: bool,
    pub loading_start_time: Option<Instant>,
    pub clear_on_protocol_receive: bool,
    pub zoom_needs_initialization: bool,
    pub last_help_toggle: Option<Instant>,
    pub brightness: i32,
    pub contrast: f32,
    prefetch_cache: PrefetchCache,
    pub slideshow_seconds: u32,
    pub slideshow_last_transition: std::time::Instant,
}

impl App {
    pub fn new(
        images: Vec<ImageSource>,
        current_index: usize,
        picker: Picker,
        filter_type: FilterType,
        scale_mode: ScaleMode,
    ) -> Result<Self, String> {
        if images.is_empty() {
            return Err("No supported images found".to_string());
        }

        let display_names: Vec<String> = images.iter().map(|img| img.display_name()).collect();
        let display_names_lowercase: Vec<String> = display_names
            .iter()
            .map(|name| name.to_lowercase())
            .collect();

        let (resize_tx, resize_rx) = mpsc::channel::<ResizeRequest>();
        let (protocol_tx, protocol_rx) = mpsc::channel::<(StatefulProtocol, (u16, u16))>();

        // Spawn background resizing worker thread
        std::thread::spawn(move || {
            let mut resizer = fir::Resizer::new();
            loop {
                if let Ok(req) = resize_rx.recv() {
                    let mut latest_req = req;
                    while let Ok(next_req) = resize_rx.try_recv() {
                        latest_req = next_req;
                    }
                    let rendered_cells = latest_req.rendered_size_cells;
                    let protocol = process_resize(latest_req, &mut resizer);
                    let _ = protocol_tx.send((protocol, rendered_cells));
                }
            }
        });

        let (loader_tx, loader_rx) = mpsc::channel::<LoaderRequest>();
        let (response_tx, response_rx) = mpsc::channel::<LoaderResponse>();

        // Spawn persistent background loader thread
        std::thread::spawn(move || {
            loop {
                if let Ok(req) = loader_rx.recv() {
                    let mut requests = vec![req];
                    while let Ok(r) = loader_rx.try_recv() {
                        requests.push(r);
                    }

                    // Find the highest sequence number
                    let highest_seq = requests.iter().map(|r| r.sequence).max().unwrap_or(0);

                    // Keep only requests matching the highest sequence
                    let mut current_requests: Vec<LoaderRequest> = requests
                        .into_iter()
                        .filter(|r| r.sequence == highest_seq)
                        .collect();

                    // Sort current_requests so that the active load (is_prefetch == false) is processed first
                    current_requests.sort_by_key(|r| r.is_prefetch);

                    for r in current_requests {
                        let res = decode_image_source(r.source);
                        let _ = response_tx.send(LoaderResponse {
                            idx: r.idx,
                            result: res,
                            is_prefetch: r.is_prefetch,
                            sequence: r.sequence,
                        });
                    }
                }
            }
        });

        let mut app = Self {
            images,
            display_names,
            display_names_lowercase,
            current_index,
            original_image: None,
            image_protocol: None,
            picker,
            img_width: 0,
            img_height: 0,
            zoom_factor: 1.0,
            pan_offset: (0, 0),
            running: true,
            show_help: false,
            error_message: None,
            last_widget_size: (0, 0),
            needs_update: true,
            needs_clear: true,
            needs_clear_once: false,
            rendered_size_cells: (0, 0),
            current_zoom_pct: 100.0,
            palette_mode: PaletteMode::Closed,
            palette_query: String::new(),
            palette_selected_index: 0,
            palette_width: 0,
            palette_height: 0,
            prompt_type: None,
            filter_type,
            scale_mode,
            matcher: nucleo::Matcher::new(nucleo::Config::DEFAULT),
            current_icon: "\u{F021F}",
            resize_tx,
            protocol_rx,
            loader_tx,
            response_rx,
            current_sequence: 0,
            is_loading: false,
            loading_start_time: None,
            clear_on_protocol_receive: false,
            zoom_needs_initialization: false,
            last_help_toggle: None,
            brightness: 0,
            contrast: 0.0,
            prefetch_cache: Arc::new(Mutex::new(HashMap::new())),
            slideshow_seconds: 0,
            slideshow_last_transition: std::time::Instant::now(),
        };

        app.start_load_image();
        Ok(app)
    }

    pub fn get_filtered_files(&mut self) -> Vec<(usize, String)> {
        let query = &self.palette_query;
        if query.is_empty() {
            return self
                .display_names
                .iter()
                .enumerate()
                .map(|(idx, name)| (idx, name.clone()))
                .collect();
        }

        let pattern = nucleo::pattern::Pattern::parse(
            query,
            nucleo::pattern::CaseMatching::Ignore,
            nucleo::pattern::Normalization::Smart,
        );

        #[derive(Clone)]
        struct FileCandidate {
            index: usize,
            name: String,
        }
        impl AsRef<str> for FileCandidate {
            fn as_ref(&self) -> &str {
                &self.name
            }
        }

        let candidates: Vec<FileCandidate> = self
            .display_names_lowercase
            .iter()
            .enumerate()
            .map(|(index, name)| FileCandidate {
                index,
                name: name.clone(),
            })
            .collect();

        let mut matches = pattern.match_list(candidates, &mut self.matcher);
        matches.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.index.cmp(&b.0.index)));

        matches
            .into_iter()
            .map(|(candidate, _score)| {
                (candidate.index, self.display_names[candidate.index].clone())
            })
            .collect()
    }

    pub fn get_filtered_commands(&mut self) -> Vec<PaletteCommand> {
        let query = &self.palette_query;
        if query.is_empty() {
            let mut list = Vec::new();
            for cmd in <Command as strum::IntoEnumIterator>::iter() {
                for item in cmd.get_metadata() {
                    if item.show_in_palette {
                        list.push(PaletteCommand { cmd, item });
                    }
                }
            }
            return list;
        }

        let pattern = nucleo::pattern::Pattern::parse(
            query,
            nucleo::pattern::CaseMatching::Ignore,
            nucleo::pattern::Normalization::Smart,
        );

        #[derive(Clone)]
        struct CmdCandidate {
            cmd: Command,
            item_index: usize,
            search_text: String,
        }
        impl AsRef<str> for CmdCandidate {
            fn as_ref(&self) -> &str {
                &self.search_text
            }
        }

        let mut candidates = Vec::new();
        for cmd in <Command as strum::IntoEnumIterator>::iter() {
            for (item_index, item) in cmd.get_metadata().iter().enumerate() {
                if item.show_in_palette {
                    candidates.push(CmdCandidate {
                        cmd,
                        item_index,
                        search_text: format!("{} {}", item.name, item.description),
                    });
                }
            }
        }

        let mut matches = pattern.match_list(candidates, &mut self.matcher);
        matches.sort_by(|a, b| {
            b.1.cmp(&a.1).then_with(|| {
                (a.0.cmd as usize)
                    .cmp(&(b.0.cmd as usize))
                    .then_with(|| a.0.item_index.cmp(&b.0.item_index))
            })
        });

        matches
            .into_iter()
            .map(|(candidate, _score)| {
                let item = &candidate.cmd.get_metadata()[candidate.item_index];
                PaletteCommand {
                    cmd: candidate.cmd,
                    item,
                }
            })
            .collect()
    }

    pub fn filter_name(&self) -> &'static str {
        match self.filter_type {
            FilterType::Nearest => "Nearest",
            FilterType::Triangle => "Linear",
            FilterType::CatmullRom => "Cubic",
            FilterType::Mitchell => "Mitchell",
            FilterType::Gaussian => "Gaussian",
            FilterType::Lanczos3 => "Lanczos",
            FilterType::Hamming => "Hamming",
        }
    }

    pub fn cycle_filter(&mut self) {
        if self.is_loading {
            return;
        }
        self.filter_type = match self.filter_type {
            FilterType::Nearest => FilterType::Hamming,
            FilterType::Hamming => FilterType::Triangle,
            FilterType::Triangle => FilterType::CatmullRom,
            FilterType::CatmullRom => FilterType::Mitchell,
            FilterType::Mitchell => FilterType::Gaussian,
            FilterType::Gaussian => FilterType::Lanczos3,
            FilterType::Lanczos3 => FilterType::Nearest,
        };
        self.needs_update = true;
    }

    pub fn execute_command(&mut self, cmd: Command) {
        match cmd {
            Command::ShowHelp => {
                self.show_help = true;
                self.needs_clear = true;
            }
            Command::ResetView => self.reset_view(),
            Command::ActualSize => self.set_actual_size(),
            Command::RotateClockwise => self.rotate_clockwise(),
            Command::RotateCounterClockwise => self.rotate_counter_clockwise(),
            Command::NextImage => self.next_image(),
            Command::PreviousImage => self.prev_image(),
            Command::ZoomIn => self.zoom_in(),
            Command::ZoomOut => self.zoom_out(),
            Command::Quit => self.running = false,
            Command::SetFilterNearest => {
                self.filter_type = FilterType::Nearest;
                self.needs_update = true;
            }
            Command::SetFilterLinear => {
                self.filter_type = FilterType::Triangle;
                self.needs_update = true;
            }
            Command::SetFilterCubic => {
                self.filter_type = FilterType::CatmullRom;
                self.needs_update = true;
            }
            Command::SetFilterMitchell => {
                self.filter_type = FilterType::Mitchell;
                self.needs_update = true;
            }
            Command::SetFilterGaussian => {
                self.filter_type = FilterType::Gaussian;
                self.needs_update = true;
            }
            Command::SetFilterLanczos => {
                self.filter_type = FilterType::Lanczos3;
                self.needs_update = true;
            }
            Command::SetFilterHamming => {
                self.filter_type = FilterType::Hamming;
                self.needs_update = true;
            }
            Command::NextFilter => self.cycle_filter(),
            Command::GoToImage => self.open_prompt(PromptType::GoToImage),
            Command::SetBrightness => self.open_prompt(PromptType::SetBrightness),
            Command::SetContrast => self.open_prompt(PromptType::SetContrast),
            Command::SetScaleNone => {
                self.scale_mode = ScaleMode::None;
                self.apply_scale_mode();
            }
            Command::SetScaleShrink => {
                self.scale_mode = ScaleMode::Shrink;
                self.apply_scale_mode();
            }
            Command::SetScaleFit => {
                self.scale_mode = ScaleMode::Full;
                self.apply_scale_mode();
            }
            Command::SetScaleCrop => {
                self.scale_mode = ScaleMode::Crop;
                self.apply_scale_mode();
            }
            Command::CycleScaleMode => self.cycle_scale_mode(),
            Command::IncreaseBrightness => self.increase_brightness(),
            Command::DecreaseBrightness => self.decrease_brightness(),
            Command::IncreaseContrast => self.increase_contrast(),
            Command::DecreaseContrast => self.decrease_contrast(),
            Command::PredefinedZoomIn => self.jump_zoom_in(),
            Command::PredefinedZoomOut => self.jump_zoom_out(),
            Command::PanLeft => self.pan_left(),
            Command::PanRight => self.pan_right(),
            Command::PanUp => self.pan_up(),
            Command::PanDown => self.pan_down(),
            Command::ToggleHelp => self.toggle_help(),
            Command::CommandPalette => self.open_palette(PaletteMode::Command),
            Command::FileSearch => self.open_palette(PaletteMode::File),
            Command::SlideshowIncrease => {
                self.slideshow_seconds = self.slideshow_seconds.saturating_add(1).max(1);
                self.slideshow_last_transition = std::time::Instant::now();
            }
            Command::SlideshowDecrease => {
                self.slideshow_seconds = self.slideshow_seconds.saturating_sub(1);
                self.slideshow_last_transition = std::time::Instant::now();
            }
            Command::SetSlideshow => self.open_prompt(PromptType::SetSlideshow),
        }
    }

    pub fn open_palette(&mut self, mode: PaletteMode) {
        self.palette_mode = mode;
        self.palette_query.clear();
        self.palette_selected_index = match mode {
            PaletteMode::File => self.current_index,
            PaletteMode::Command => 0,
            _ => 0,
        };
        self.needs_clear = true;

        let max_text_width = match mode {
            PaletteMode::File => self
                .display_names
                .iter()
                .map(|name| name.len())
                .max()
                .unwrap_or(0) as u16,
            PaletteMode::Command => get_commands()
                .iter()
                .filter(|cmd| cmd.show_in_palette)
                .map(|cmd| cmd.name.len() + 3 + cmd.description.len())
                .max()
                .unwrap_or(0) as u16,
            _ => 0,
        };

        self.palette_width = max_text_width + 5;
        self.palette_height = 0;
    }

    pub fn open_prompt(&mut self, prompt_type: PromptType) {
        self.palette_mode = PaletteMode::Prompt;
        self.prompt_type = Some(prompt_type);
        self.palette_query.clear();
        self.palette_selected_index = 0;
        self.palette_width = 45;
        self.palette_height = 0;
        self.needs_clear = true;
    }

    pub fn execute_prompt(&mut self, prompt_type: PromptType) {
        match prompt_type {
            PromptType::GoToImage => {
                let input = self.palette_query.trim();
                if !input.is_empty() && !self.images.is_empty() {
                    let mut new_idx = self.current_index;
                    if let Some(stripped) = input.strip_prefix('+') {
                        if let Ok(offset) = stripped.parse::<usize>() {
                            new_idx = (self.current_index + offset).min(self.images.len() - 1);
                        }
                    } else if let Some(stripped) = input.strip_prefix('-') {
                        if let Ok(offset) = stripped.parse::<usize>() {
                            new_idx = self.current_index.saturating_sub(offset);
                        }
                    } else if let Ok(Some(val_minus_1)) =
                        input.parse::<usize>().map(|v| v.checked_sub(1))
                    {
                        new_idx = val_minus_1.min(self.images.len() - 1);
                    }
                    if new_idx != self.current_index {
                        self.current_index = new_idx;
                        self.start_load_image();
                    }
                }
            }
            PromptType::SetBrightness => {
                let input = self.palette_query.trim();
                if !input.is_empty() && self.original_image.is_some() {
                    let mut new_val = self.brightness;
                    if let Some(stripped) = input.strip_prefix('+') {
                        if let Ok(offset) = stripped.parse::<i32>() {
                            new_val = self.brightness + offset;
                        }
                    } else if let Some(stripped) = input.strip_prefix('-') {
                        if let Ok(offset) = stripped.parse::<i32>() {
                            new_val = self.brightness - offset;
                        }
                    } else if let Ok(val) = input.parse::<i32>() {
                        new_val = val;
                    }
                    let new_val = new_val.clamp(-255, 255);
                    if new_val != self.brightness {
                        self.brightness = new_val;
                        self.needs_update = true;
                    }
                }
            }
            PromptType::SetContrast => {
                let input = self.palette_query.trim();
                if !input.is_empty() && self.original_image.is_some() {
                    let mut new_val = self.contrast;
                    if let Some(stripped) = input.strip_prefix('+') {
                        if let Ok(offset) = stripped.parse::<f32>() {
                            new_val = self.contrast + offset;
                        }
                    } else if let Some(stripped) = input.strip_prefix('-') {
                        if let Ok(offset) = stripped.parse::<f32>() {
                            new_val = self.contrast - offset;
                        }
                    } else if let Ok(val) = input.parse::<f32>() {
                        new_val = val;
                    }
                    let new_val = new_val.clamp(-255.0, 255.0);
                    if (new_val - self.contrast).abs() > f32::EPSILON {
                        self.contrast = new_val;
                        self.needs_update = true;
                    }
                }
            }
            PromptType::SetSlideshow => {
                let input = self.palette_query.trim();
                if !input.is_empty() {
                    let mut new_val = self.slideshow_seconds;
                    if let Some(stripped) = input.strip_prefix('+') {
                        if let Ok(offset) = stripped.parse::<u32>() {
                            new_val = self.slideshow_seconds.saturating_add(offset);
                        }
                    } else if let Some(stripped) = input.strip_prefix('-') {
                        if let Ok(offset) = stripped.parse::<u32>() {
                            new_val = self.slideshow_seconds.saturating_sub(offset);
                        }
                    } else if let Ok(val) = input.parse::<u32>() {
                        new_val = val;
                    }
                    if new_val != self.slideshow_seconds {
                        self.slideshow_seconds = new_val;
                        self.slideshow_last_transition = std::time::Instant::now();
                    }
                }
            }
        }
        self.palette_mode = PaletteMode::Closed;
        self.prompt_type = None;
        self.needs_clear_once = true;
    }

    /// Start loading the image at the current index in the background
    /// Trigger background prefetching of adjacent images and prune old cache entries
    pub fn get_sliding_window_indices(&self) -> Vec<usize> {
        let n = 2; // Cache size N=2 (caches current + 2 before + 2 after)
        let total = self.images.len();
        if total == 0 {
            return Vec::new();
        }
        let mut indices = Vec::new();
        indices.push(self.current_index);
        for i in 1..=n {
            let prev = (self.current_index + total - i) % total;
            let next = (self.current_index + i) % total;
            indices.push(prev);
            indices.push(next);
        }
        indices.sort();
        indices.dedup();
        indices
    }

    pub fn trigger_prefetch(&self) {
        if self.images.len() <= 1 {
            return;
        }

        let window_indices = self.get_sliding_window_indices();

        // Prune any cache entries that are not in the sliding window
        {
            let mut cache = self.prefetch_cache.lock().unwrap();
            cache.retain(|idx, _| window_indices.contains(idx));
        }

        for idx in window_indices {
            if idx == self.current_index {
                continue;
            }

            {
                let cache = self.prefetch_cache.lock().unwrap();
                if cache.contains_key(&idx) {
                    continue;
                }
            }

            let source = self.images[idx].clone();
            let _ = self.loader_tx.send(LoaderRequest {
                idx,
                source,
                is_prefetch: true,
                sequence: self.current_sequence,
            });
        }
    }

    /// Start loading the image at the current index in the background
    pub fn start_load_image(&mut self) {
        if self.images.is_empty() {
            self.original_image = None;
            self.image_protocol = None;
            self.error_message = Some("No supported images found".to_string());
            return;
        }

        self.error_message = None;
        self.clear_on_protocol_receive = true;
        self.slideshow_last_transition = std::time::Instant::now();

        // Check if the image is in the prefetch cache
        let cached = {
            let mut cache = self.prefetch_cache.lock().unwrap();
            cache.remove(&self.current_index)
        };

        if let Some((img, w, h, icon)) = cached {
            self.current_sequence += 1;
            self.original_image = Some(img);
            self.current_icon = icon;
            self.img_width = w;
            self.img_height = h;
            self.zoom_factor = 1.0;
            self.pan_offset = (0, 0);
            self.brightness = 0;
            self.contrast = 0.0;
            self.is_loading = false;
            self.needs_update = true;
            self.zoom_needs_initialization = true;
            self.trigger_prefetch();
            return;
        }

        // Cache miss: load as normal via background loader worker
        self.original_image = None;
        self.image_protocol = None;
        self.is_loading = true;
        self.loading_start_time = Some(Instant::now());
        self.current_sequence += 1;

        let source = self.images[self.current_index].clone();
        let _ = self.loader_tx.send(LoaderRequest {
            idx: self.current_index,
            source,
            is_prefetch: false,
            sequence: self.current_sequence,
        });

        // Trigger prefetching immediately under this new sequence
        self.trigger_prefetch();
    }

    /// Check background threads and poll their messages
    pub fn update_channels(&mut self) {
        while let Ok(resp) = self.response_rx.try_recv() {
            if resp.sequence < self.current_sequence {
                continue;
            }

            match resp.result {
                Ok((img, w, h, icon)) => {
                    let shared_img = Arc::new(img);
                    if resp.is_prefetch {
                        let window_indices = self.get_sliding_window_indices();
                        if window_indices.contains(&resp.idx) {
                            let mut cache = self.prefetch_cache.lock().unwrap();
                            cache.insert(resp.idx, (shared_img, w, h, icon));
                        }
                    } else if resp.idx == self.current_index {
                        self.img_width = w;
                        self.img_height = h;
                        self.current_icon = icon;
                        self.original_image = Some(shared_img);
                        self.error_message = None;
                        self.zoom_factor = 1.0;
                        self.pan_offset = (0, 0);
                        self.brightness = 0;
                        self.contrast = 0.0;
                        self.needs_update = true;
                        self.zoom_needs_initialization = true;
                        self.trigger_prefetch();
                    }
                }
                Err(err) => {
                    if !resp.is_prefetch && resp.idx == self.current_index {
                        self.original_image = None;
                        self.image_protocol = None;
                        self.error_message = Some(err);
                        self.is_loading = false;
                    }
                }
            }
        }

        if let Ok((protocol, cells)) = self.protocol_rx.try_recv() {
            self.image_protocol = Some(protocol);
            self.rendered_size_cells = cells;
            self.is_loading = false;
            if self.clear_on_protocol_receive {
                self.clear_on_protocol_receive = false;
                self.needs_clear = true;
            }
        }
    }

    /// Update the ratatui-image protocol state based on zoom and pan (processed asynchronously)
    pub fn update_protocol(&mut self, widget_w: u16, widget_h: u16) {
        if widget_w == 0 || widget_h == 0 {
            return;
        }

        let font_size = self.picker.font_size();
        let mut cell_w = font_size.width;
        let mut cell_h = font_size.height;
        if cell_w == 0 {
            cell_w = 8;
        }
        if cell_h == 0 {
            cell_h = 16;
        }

        let widget_w_px = widget_w as f64 * cell_w as f64;
        let widget_h_px = widget_h as f64 * cell_h as f64;

        if let Some(ref img) = self.original_image {
            let w_orig = self.img_width as f64;
            let h_orig = self.img_height as f64;

            // 1. Calculate fit-to-screen scale 's'
            let s_w = widget_w_px / w_orig;
            let s_h = widget_h_px / h_orig;
            let s = s_w.min(s_h);

            if self.zoom_needs_initialization && s > 0.0 {
                self.zoom_needs_initialization = false;
                self.zoom_factor = match self.scale_mode {
                    ScaleMode::None => 1.0 / s,
                    ScaleMode::Shrink => {
                        if s < 1.0 {
                            1.0
                        } else {
                            1.0 / s
                        }
                    }
                    ScaleMode::Full => 1.0,
                    ScaleMode::Crop => s_w.max(s_h) / s,
                };
                self.pan_offset = (0, 0);
            }

            // 2. Combined scale is s * zoom_factor
            let scale = s * self.zoom_factor;
            self.current_zoom_pct = scale * 100.0;

            // 3. Compute crop window in original image pixels
            let crop_w = (widget_w_px / scale).round() as u32;
            let crop_h = (widget_h_px / scale).round() as u32;
            let crop_w = crop_w.max(1);
            let crop_h = crop_h.max(1);

            // Calculate target rendering size in pixels based on crop and zoom scale
            let target_w = (crop_w as f64 * scale).round() as u32;
            let target_h = (crop_h as f64 * scale).round() as u32;
            let target_w = target_w.max(1);
            let target_h = target_h.max(1);

            // Center of crop window is center of image + pan_offset
            let center_x = (self.img_width as i64 / 2) + self.pan_offset.0;
            let center_y = (self.img_height as i64 / 2) + self.pan_offset.1;

            // Compute top-left of crop box (can be negative if we pan past bounds)
            let crop_x1 = center_x - (crop_w as i64 / 2);
            let crop_y1 = center_y - (crop_h as i64 / 2);
            let crop_x2 = crop_x1 + crop_w as i64;
            let crop_y2 = crop_y1 + crop_h as i64;

            // Intersecting bounds with original image
            let inter_x1 = crop_x1.clamp(0, self.img_width as i64);
            let inter_y1 = crop_y1.clamp(0, self.img_height as i64);
            let inter_x2 = crop_x2.clamp(0, self.img_width as i64);
            let inter_y2 = crop_y2.clamp(0, self.img_height as i64);

            // Calculate exact cell size of the rendered image
            let cells_w = (target_w as f64 / cell_w as f64).round() as u16;
            let cells_h = (target_h as f64 / cell_h as f64).round() as u16;
            let rendered_cells = (cells_w.clamp(1, widget_w), cells_h.clamp(1, widget_h));

            let req = ResizeRequest {
                img: Arc::clone(img),
                scale,
                crop_x1,
                crop_y1,
                crop_x2,
                crop_y2,
                inter_x1,
                inter_y1,
                inter_x2,
                inter_y2,
                target_w,
                target_h,
                filter_type: self.filter_type,
                picker: self.picker.clone(),
                brightness: self.brightness,
                contrast: self.contrast,
                rendered_size_cells: rendered_cells,
            };

            let _ = self.resize_tx.send(req);
        } else {
            self.image_protocol = None;
            self.rendered_size_cells = (0, 0);
        }
    }

    fn get_fit_scale(&self) -> f64 {
        let (widget_w_cells, widget_h_cells) = self.last_widget_size;
        if widget_w_cells == 0 || widget_h_cells == 0 {
            return 0.0;
        }
        let font_size = self.picker.font_size();
        let mut cell_w = font_size.width;
        let mut cell_h = font_size.height;
        if cell_w == 0 {
            cell_w = 8;
        }
        if cell_h == 0 {
            cell_h = 16;
        }

        let widget_w_px = widget_w_cells as f64 * cell_w as f64;
        let widget_h_px = widget_h_cells as f64 * cell_h as f64;

        let s_w = widget_w_px / self.img_width as f64;
        let s_h = widget_h_px / self.img_height as f64;
        s_w.min(s_h)
    }

    /// Detect if we should sweep the screen with clear() to avoid graphics overlap artifacts.
    /// Only necessary for Sixel terminals (like Foot) which write directly to cell grids.
    pub fn should_clear_on_update(&self) -> bool {
        matches!(self.picker.protocol_type(), ProtocolType::Sixel)
    }

    /// Zoom in
    pub fn zoom_in(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        let s = self.get_fit_scale();
        if s > 0.0 {
            self.zoom_factor = (self.zoom_factor * 1.25).min(102.4 / s);
            self.clamp_pan();
            self.needs_update = true;
        }
    }

    /// Zoom out
    pub fn zoom_out(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        let s = self.get_fit_scale();
        if s > 0.0 {
            self.zoom_factor = (self.zoom_factor / 1.25).max(0.01 / s);
            self.clamp_pan();
            self.needs_update = true;
        }
    }

    /// Predefined Zoom In
    pub fn jump_zoom_in(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        let fit_scale = self.get_fit_scale();
        if fit_scale <= 0.0 {
            return;
        }

        // Calculate crop scale (crop to fill)
        let (widget_w_cells, widget_h_cells) = self.last_widget_size;
        let font_size = self.picker.font_size();
        let mut cell_w = font_size.width;
        let mut cell_h = font_size.height;
        if cell_w == 0 {
            cell_w = 8;
        }
        if cell_h == 0 {
            cell_h = 16;
        }
        let widget_w_px = widget_w_cells as f64 * cell_w as f64;
        let widget_h_px = widget_h_cells as f64 * cell_h as f64;
        let s_w = widget_w_px / self.img_width as f64;
        let s_h = widget_h_px / self.img_height as f64;
        let crop_scale = s_w.max(s_h);

        // Predefined levels in terms of absolute scale (scale = fit_scale * zoom_factor)
        let shrink_to_fit_scale = fit_scale.min(1.0);
        let fit_view_scale = fit_scale;
        let crop_to_fill_scale = crop_scale;
        let one_to_one_scale = 1.0;
        let two_to_one_scale = 2.0;
        let four_to_one_scale = 4.0;

        let mut levels = vec![
            shrink_to_fit_scale,
            fit_view_scale,
            crop_to_fill_scale,
            one_to_one_scale,
            two_to_one_scale,
            four_to_one_scale,
        ];
        levels.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        levels.dedup_by(|a, b| (*a - *b).abs() < 0.01);

        let current_scale = fit_scale * self.zoom_factor;

        let mut target_scale = None;
        for &lvl in &levels {
            if lvl > current_scale + 0.01 {
                target_scale = Some(lvl);
                break;
            }
        }

        if let Some(target) = target_scale {
            self.zoom_factor = target / fit_scale;
        } else {
            // Double the scale if already past maximum level
            self.zoom_factor = (current_scale * 2.0).min(102.4) / fit_scale;
        }

        self.clamp_pan();
        self.needs_update = true;
    }

    /// Predefined Zoom Out
    pub fn jump_zoom_out(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        let fit_scale = self.get_fit_scale();
        if fit_scale <= 0.0 {
            return;
        }

        // Calculate crop scale (crop to fill)
        let (widget_w_cells, widget_h_cells) = self.last_widget_size;
        let font_size = self.picker.font_size();
        let mut cell_w = font_size.width;
        let mut cell_h = font_size.height;
        if cell_w == 0 {
            cell_w = 8;
        }
        if cell_h == 0 {
            cell_h = 16;
        }
        let widget_w_px = widget_w_cells as f64 * cell_w as f64;
        let widget_h_px = widget_h_cells as f64 * cell_h as f64;
        let s_w = widget_w_px / self.img_width as f64;
        let s_h = widget_h_px / self.img_height as f64;
        let crop_scale = s_w.max(s_h);

        // Predefined levels in terms of absolute scale (scale = fit_scale * zoom_factor)
        let shrink_to_fit_scale = fit_scale.min(1.0);
        let fit_view_scale = fit_scale;
        let crop_to_fill_scale = crop_scale;
        let one_to_one_scale = 1.0;
        let two_to_one_scale = 2.0;
        let four_to_one_scale = 4.0;

        let mut levels = vec![
            shrink_to_fit_scale,
            fit_view_scale,
            crop_to_fill_scale,
            one_to_one_scale,
            two_to_one_scale,
            four_to_one_scale,
        ];
        levels.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        levels.dedup_by(|a, b| (*a - *b).abs() < 0.01);

        let current_scale = fit_scale * self.zoom_factor;

        let mut target_scale = None;
        for &lvl in levels.iter().rev() {
            if lvl < current_scale - 0.01 {
                target_scale = Some(lvl);
                break;
            }
        }

        if let Some(target) = target_scale {
            self.zoom_factor = target / fit_scale;
        } else {
            // Halve the scale if already below minimum level
            self.zoom_factor = (current_scale / 2.0).max(0.01) / fit_scale;
        }

        self.clamp_pan();
        self.needs_update = true;
    }

    /// Actual size (100% zoom)
    pub fn set_actual_size(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        let s = self.get_fit_scale();
        if s > 0.0 {
            // Actual size scale is 1.0 (100%). Since scale = s * zoom_factor,
            // we want s * zoom_factor = 1.0 => zoom_factor = 1.0 / s
            self.zoom_factor = 1.0 / s;
            self.clamp_pan();
            self.needs_update = true;
            self.clear_on_protocol_receive = true;
        }
    }

    /// Reset zoom, pan, brightness, and contrast
    pub fn reset_view(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        self.apply_scale_mode();
        self.brightness = 0;
        self.contrast = 0.0;
        self.clear_on_protocol_receive = true;
    }

    pub fn apply_scale_mode(&mut self) {
        let s = self.get_fit_scale();
        if s > 0.0 {
            self.zoom_factor = match self.scale_mode {
                ScaleMode::None => 1.0 / s,
                ScaleMode::Shrink => {
                    if s < 1.0 {
                        1.0
                    } else {
                        1.0 / s
                    }
                }
                ScaleMode::Full => 1.0,
                ScaleMode::Crop => {
                    let (widget_w_cells, widget_h_cells) = self.last_widget_size;
                    let font_size = self.picker.font_size();
                    let mut cell_w = font_size.width;
                    let mut cell_h = font_size.height;
                    if cell_w == 0 {
                        cell_w = 8;
                    }
                    if cell_h == 0 {
                        cell_h = 16;
                    }
                    let widget_w_px = widget_w_cells as f64 * cell_w as f64;
                    let widget_h_px = widget_h_cells as f64 * cell_h as f64;
                    let s_w = widget_w_px / self.img_width as f64;
                    let s_h = widget_h_px / self.img_height as f64;
                    s_w.max(s_h) / s
                }
            };
            self.pan_offset = (0, 0);
            self.needs_update = true;
        }
    }

    pub fn cycle_scale_mode(&mut self) {
        if self.is_loading {
            return;
        }
        self.scale_mode = match self.scale_mode {
            ScaleMode::None => ScaleMode::Shrink,
            ScaleMode::Shrink => ScaleMode::Full,
            ScaleMode::Full => ScaleMode::Crop,
            ScaleMode::Crop => ScaleMode::None,
        };
        self.apply_scale_mode();
    }

    pub fn toggle_help(&mut self) {
        let now = Instant::now();
        if let Some(last) = self.last_help_toggle
            && now.duration_since(last) < std::time::Duration::from_millis(150)
        {
            return;
        }
        self.last_help_toggle = Some(now);
        self.show_help = !self.show_help;
        if !self.show_help {
            self.needs_update = true;
            self.needs_clear_once = true;
        }
        self.needs_clear = true;
    }

    pub fn increase_brightness(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        self.brightness = (self.brightness + 10).min(255);
        self.needs_update = true;
    }

    pub fn decrease_brightness(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        self.brightness = (self.brightness - 10).max(-255);
        self.needs_update = true;
    }

    pub fn increase_contrast(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        self.contrast = (self.contrast + 10.0).min(255.0);
        self.needs_update = true;
    }

    pub fn decrease_contrast(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        self.contrast = (self.contrast - 10.0).max(-255.0);
        self.needs_update = true;
    }

    /// Clamp pan offsets so that the corners of the image cannot pan past the center point of the viewport
    pub fn clamp_pan(&mut self) {
        if self.original_image.is_none() {
            return;
        }
        let max_pan_x = (self.img_width as i64 / 2).max(0);
        let max_pan_y = (self.img_height as i64 / 2).max(0);

        self.pan_offset.0 = self.pan_offset.0.clamp(-max_pan_x, max_pan_x);
        self.pan_offset.1 = self.pan_offset.1.clamp(-max_pan_y, max_pan_y);
    }

    /// Pan left
    pub fn pan_left(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        let step = self.pan_step_x();
        self.pan_offset.0 -= step;
        self.clamp_pan();
        self.needs_update = true;
    }

    /// Pan right
    pub fn pan_right(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        let step = self.pan_step_x();
        self.pan_offset.0 += step;
        self.clamp_pan();
        self.needs_update = true;
    }

    /// Pan up
    pub fn pan_up(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        let step = self.pan_step_y();
        self.pan_offset.1 -= step;
        self.clamp_pan();
        self.needs_update = true;
    }

    /// Pan down
    pub fn pan_down(&mut self) {
        if self.original_image.is_none() || self.is_loading {
            return;
        }
        let step = self.pan_step_y();
        self.pan_offset.1 += step;
        self.clamp_pan();
        self.needs_update = true;
    }

    fn pan_step_x(&self) -> i64 {
        let s = self.get_fit_scale();
        let scale = if s > 0.0 { s * self.zoom_factor } else { 1.0 };
        ((self.img_width as f64 * 0.05) / scale).max(1.0) as i64
    }

    fn pan_step_y(&self) -> i64 {
        let s = self.get_fit_scale();
        let scale = if s > 0.0 { s * self.zoom_factor } else { 1.0 };
        ((self.img_height as f64 * 0.05) / scale).max(1.0) as i64
    }

    /// Rotate 90 degrees clockwise
    pub fn rotate_clockwise(&mut self) {
        if self.is_loading {
            return;
        }
        if let Some(img) = self.original_image.take() {
            let rotated = img.rotate90();
            self.img_width = rotated.width();
            self.img_height = rotated.height();
            self.original_image = Some(Arc::new(rotated));
            self.zoom_factor = 1.0;
            self.pan_offset = (0, 0);
            self.needs_update = true;
            self.clear_on_protocol_receive = true;
        }
    }

    /// Rotate 90 degrees counter-clockwise
    pub fn rotate_counter_clockwise(&mut self) {
        if self.is_loading {
            return;
        }
        if let Some(img) = self.original_image.take() {
            let rotated = img.rotate270();
            self.img_width = rotated.width();
            self.img_height = rotated.height();
            self.original_image = Some(Arc::new(rotated));
            self.zoom_factor = 1.0;
            self.pan_offset = (0, 0);
            self.needs_update = true;
            self.clear_on_protocol_receive = true;
        }
    }

    /// Get current image file name
    pub fn current_filename(&self) -> &str {
        if self.images.is_empty() {
            return "No file loaded";
        }
        &self.display_names[self.current_index]
    }

    /// Next image
    pub fn next_image(&mut self) {
        if self.images.is_empty() {
            return;
        }
        self.current_index = (self.current_index + 1) % self.images.len();
        self.start_load_image();
    }

    /// Previous image
    pub fn prev_image(&mut self) {
        if self.images.is_empty() {
            return;
        }
        if self.current_index == 0 {
            self.current_index = self.images.len() - 1;
        } else {
            self.current_index -= 1;
        }
        self.start_load_image();
    }
}

/// Scan target path and sibling/child images
fn scan_directory(initial_path: &Path, check_magic: bool) -> Result<(Vec<PathBuf>, usize), String> {
    let (dir, file_name) = if initial_path.is_file() {
        let parent = initial_path.parent().unwrap_or_else(|| Path::new("."));
        let name = initial_path.file_name().map(|n| n.to_os_string());
        (parent.to_path_buf(), name)
    } else if initial_path.is_dir() {
        (initial_path.to_path_buf(), None)
    } else {
        // Fallback to checking if it is in relative path
        let parent = Path::new(".");
        if parent.join(initial_path).is_file() {
            (
                parent.to_path_buf(),
                Some(initial_path.as_os_str().to_os_string()),
            )
        } else {
            return Err(format!("Path does not exist: {}", initial_path.display()));
        }
    };

    let mut images = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if is_image_file(&path, check_magic) {
                images.push(path);
            }
        }
    }

    images.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

    let index = if let Some(ref target_name) = file_name {
        images
            .iter()
            .position(|path| path.file_name().map(|n| n == target_name).unwrap_or(false))
            .unwrap_or_else(|| {
                if initial_path.exists() {
                    images.push(initial_path.to_path_buf());
                    images.len() - 1
                } else {
                    0
                }
            })
    } else {
        0
    };

    Ok((images, index))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuessType {
    Image,
    Zip,
}

fn guess_file_type(path: &Path) -> Option<GuessType> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut header = [0u8; 16];
    use std::io::Read;
    let n = file.read(&mut header).ok()?;
    let bytes = &header[..n];

    if bytes.len() >= 2 && bytes[0] == 0x50 && bytes[1] == 0x4B {
        return Some(GuessType::Zip);
    }

    if image::guess_format(bytes).is_ok() {
        return Some(GuessType::Image);
    }

    None
}

fn is_image_file(path: &Path, check_magic: bool) -> bool {
    if !path.is_file() {
        return false;
    }
    if let Some(ext) = path.extension().and_then(|e| e.to_str())
        && (ext.eq_ignore_ascii_case("png")
            || ext.eq_ignore_ascii_case("jpg")
            || ext.eq_ignore_ascii_case("jpeg")
            || ext.eq_ignore_ascii_case("gif")
            || ext.eq_ignore_ascii_case("webp")
            || ext.eq_ignore_ascii_case("bmp")
            || ext.eq_ignore_ascii_case("tiff")
            || ext.eq_ignore_ascii_case("ico"))
    {
        return true;
    }
    if check_magic {
        matches!(guess_file_type(path), Some(GuessType::Image))
    } else {
        false
    }
}

fn is_cbz_or_zip(path: &Path, check_magic: bool) -> bool {
    if !path.is_file() {
        return false;
    }
    if let Some(ext) = path.extension().and_then(|e| e.to_str())
        && (ext.eq_ignore_ascii_case("cbz") || ext.eq_ignore_ascii_case("zip"))
    {
        return true;
    }
    if check_magic {
        matches!(guess_file_type(path), Some(GuessType::Zip))
    } else {
        false
    }
}

fn list_cbz_pages(zip_path: &Path) -> Result<Vec<String>, String> {
    let file = std::fs::File::open(zip_path)
        .map_err(|e| format!("Failed to open zip file {}: {}", zip_path.display(), e))?;
    let reader = std::io::BufReader::new(file);
    let mut archive = zip::ZipArchive::new(reader)
        .map_err(|e| format!("Failed to read zip archive {}: {}", zip_path.display(), e))?;

    let mut pages = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i)
            && entry.is_file()
        {
            let name = entry.name();
            if let Some(ext) = Path::new(name).extension().and_then(|e| e.to_str())
                && (ext.eq_ignore_ascii_case("png")
                    || ext.eq_ignore_ascii_case("jpg")
                    || ext.eq_ignore_ascii_case("jpeg")
                    || ext.eq_ignore_ascii_case("gif")
                    || ext.eq_ignore_ascii_case("webp")
                    || ext.eq_ignore_ascii_case("bmp")
                    || ext.eq_ignore_ascii_case("tiff")
                    || ext.eq_ignore_ascii_case("ico"))
            {
                pages.push(name.to_string());
            }
        }
    }

    pages.sort_by_cached_key(|a| a.to_lowercase());
    Ok(pages)
}

fn collect_sources(paths: &[PathBuf], check_magic: bool) -> Result<Vec<ImageSource>, String> {
    let mut sources = Vec::with_capacity(paths.len());
    for path in paths {
        if is_cbz_or_zip(path, check_magic) {
            let pages = list_cbz_pages(path)?;
            for page in pages {
                sources.push(ImageSource::Cbz {
                    zip_path: path.clone(),
                    file_in_zip: page,
                });
            }
        } else if is_image_file(path, check_magic) {
            sources.push(ImageSource::Local(path.clone()));
        }
    }
    Ok(sources)
}

fn ui(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(frame.area());

    // Update protocol if size has changed or update is requested
    let widget_size = (chunks[0].width, chunks[0].height);
    if app.needs_update || app.last_widget_size != widget_size {
        app.last_widget_size = widget_size;
        app.needs_update = false;
        app.update_protocol(widget_size.0, widget_size.1);
    }

    // Render image or placeholders
    let show_loading = app.is_loading
        && (app.image_protocol.is_none()
            || app
                .loading_start_time
                .is_some_and(|t| t.elapsed() > Duration::from_millis(150)));

    if show_loading {
        let loading_paragraph = Paragraph::new("\n\nLoading Image...")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::Yellow).bold());
        frame.render_widget(loading_paragraph, chunks[0]);
    } else if let Some(ref mut protocol) = app.image_protocol {
        // Calculate the centered Rect inside chunks[0]
        let (rect_w, rect_h) = app.rendered_size_cells;
        let x = chunks[0].x + (chunks[0].width.saturating_sub(rect_w)) / 2;
        let y = chunks[0].y + (chunks[0].height.saturating_sub(rect_h)) / 2;
        let centered_rect = Rect::new(x, y, rect_w, rect_h);

        let image_widget = StatefulImage::default();
        frame.render_stateful_widget(image_widget, centered_rect, protocol);
    } else if let Some(ref err) = app.error_message {
        let err_block = Block::default()
            .borders(Borders::ALL)
            .title(" Error Loading Image ")
            .style(Style::default().fg(Color::Red));
        let err_paragraph = Paragraph::new(err.as_str())
            .block(err_block)
            .alignment(Alignment::Center);
        frame.render_widget(err_paragraph, chunks[0]);
    } else {
        let loading_paragraph = Paragraph::new("No image loaded.")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::Yellow));
        frame.render_widget(loading_paragraph, chunks[0]);
    }

    if app.images.is_empty() {
        let status_block = Block::default()
            .title(" imv-tui ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Cyan))
            .title_style(Style::default().fg(Color::Yellow).bold());
        let inner_rect = status_block.inner(chunks[1]);
        frame.render_widget(status_block, chunks[1]);
        let empty_para = Paragraph::new(" No files found. Press 'q' to quit. ")
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::White).bg(Color::Reset));
        frame.render_widget(empty_para, inner_rect);
    } else {
        let mut extra_info = String::new();
        if app.brightness != 0 {
            extra_info.push_str(&format!(" | Brightness: {:+}", app.brightness));
        }
        if app.contrast != 0.0 {
            extra_info.push_str(&format!(" | Contrast: {:+}%", app.contrast.round() as i32));
        }
        if app.slideshow_seconds > 0 {
            extra_info.push_str(&format!(" | Slideshow: {}s", app.slideshow_seconds));
        }

        let title_text = format!(" {} {} ", app.current_icon, app.current_filename());
        let status_block = Block::default()
            .title(title_text)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Cyan))
            .title_style(Style::default().fg(Color::Yellow).bold());
        let inner_rect = status_block.inner(chunks[1]);
        frame.render_widget(status_block, chunks[1]);

        let status_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(30),
                Constraint::Min(0),
                Constraint::Length(22),
            ])
            .split(inner_rect);

        let left_text = format!(
            " [{}/{}] ({}x{}) ",
            app.current_index + 1,
            app.images.len(),
            app.img_width,
            app.img_height
        );
        let left_para = Paragraph::new(left_text)
            .alignment(Alignment::Left)
            .style(Style::default().fg(Color::White).bg(Color::Reset));
        frame.render_widget(left_para, status_chunks[0]);

        let mid_text = format!(
            "Scale: {} | Filter: {} | Zoom: {}% | Pan: ({}, {}){}",
            app.scale_mode.name(),
            app.filter_name(),
            app.current_zoom_pct.round() as i64,
            app.pan_offset.0,
            app.pan_offset.1,
            extra_info
        );
        let mid_para = Paragraph::new(mid_text)
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::White).bg(Color::Reset));
        frame.render_widget(mid_para, status_chunks[1]);

        let right_text = "Press '?' for help ";
        let right_para = Paragraph::new(right_text)
            .alignment(Alignment::Right)
            .style(Style::default().fg(Color::White).bg(Color::Reset));
        frame.render_widget(right_para, status_chunks[2]);
    }

    // Help Popup overlay
    if app.show_help {
        let mut help_lines = vec![
            Line::from(" imv-tui Keyboard Shortcuts ".bold().yellow()),
            Line::from(" ───────────────────────────────── ".gray()),
        ];
        let mut printed_brightness = false;
        let mut printed_contrast = false;
        let mut printed_pan_vim = false;
        let mut printed_pan_arrows = false;
        let mut printed_slideshow = false;

        for cmd in <Command as strum::IntoEnumIterator>::iter() {
            for def in cmd.get_metadata() {
                match def.group {
                    CommandGroup::Hidden => {}
                    CommandGroup::Normal(bindings) => {
                        if !bindings.is_empty() {
                            let shortcut = bindings
                                .iter()
                                .map(|b| b.format())
                                .collect::<Vec<_>>()
                                .join(", ");
                            help_lines.push(Line::from(vec![
                                format!("  {:<15}", shortcut).cyan(),
                                format!("- {}", def.description).into(),
                            ]));
                        }
                    }
                    CommandGroup::Brightness(_) => {
                        if !printed_brightness {
                            printed_brightness = true;
                            help_lines.push(Line::from(vec![
                                format!("  {:<15}", "b, B").cyan(),
                                "- Brightness +/-".into(),
                            ]));
                        }
                    }
                    CommandGroup::Contrast(_) => {
                        if !printed_contrast {
                            printed_contrast = true;
                            help_lines.push(Line::from(vec![
                                format!("  {:<15}", "c, C").cyan(),
                                "- Contrast +/-".into(),
                            ]));
                        }
                    }
                    CommandGroup::Pan(bindings) => {
                        if let Some(first_bind) = bindings.first() {
                            match first_bind {
                                KeyDef::Char(_) if !printed_pan_vim => {
                                    printed_pan_vim = true;
                                    help_lines.push(Line::from(vec![
                                        format!("  {:<15}", "h, j, k, l").cyan(),
                                        "- Pan Left/Down/Up/Right".into(),
                                    ]));
                                }
                                KeyDef::Code(_) if !printed_pan_arrows => {
                                    printed_pan_arrows = true;
                                    help_lines.push(Line::from(vec![
                                        format!("  {:<15}", "Arrow Keys").cyan(),
                                        "- Pan image".into(),
                                    ]));
                                }
                                _ => {}
                            }
                        }
                    }
                    CommandGroup::Slideshow(_) => {
                        if !printed_slideshow {
                            printed_slideshow = true;
                            help_lines.push(Line::from(vec![
                                format!("  {:<15}", "t, T").cyan(),
                                "- Slideshow +/- 1s".into(),
                            ]));
                        }
                    }
                }
            }
        }

        let help_paragraph = Paragraph::new(help_lines)
            .block(
                Block::default()
                    .title(" Help ")
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title_style(Style::default().fg(Color::Yellow).bold()),
            )
            .style(Style::default().fg(Color::White).bg(Color::Reset));

        let help_width = 44_u16;
        let help_height = 23_u16;

        let w = help_width.min(chunks[0].width.saturating_sub(1));
        let h = help_height.min(chunks[0].height.saturating_sub(1));
        let x = chunks[0].x + chunks[0].width.saturating_sub(w).saturating_sub(1);
        let y = chunks[0].y.saturating_add(1);

        let popup_area = Rect::new(x, y, w, h);
        frame.render_widget(Clear, popup_area);
        frame.render_widget(help_paragraph, popup_area);
    }

    // Command / File Palette popup
    if app.palette_mode != PaletteMode::Closed {
        if app.palette_mode == PaletteMode::Prompt {
            let prompt_title = match app.prompt_type {
                Some(PromptType::GoToImage) => " Go to Image ",
                Some(PromptType::SetBrightness) => " Set Brightness ",
                Some(PromptType::SetContrast) => " Set Contrast ",
                Some(PromptType::SetSlideshow) => " Set Slideshow ",
                None => " Input ",
            };
            let prompt_label = match app.prompt_type {
                Some(PromptType::GoToImage) => "Enter index (e.g. 40, +10, -10):",
                Some(PromptType::SetBrightness) => "Enter brightness (e.g. 50, +10, -10):",
                Some(PromptType::SetContrast) => "Enter contrast % (e.g. 20, +5, -5):",
                Some(PromptType::SetSlideshow) => {
                    "Enter slideshow delay in seconds (e.g. 5, +1, -1):"
                }
                None => "Enter value:",
            };

            let lines = vec![
                Line::from(format!("   {}", prompt_label).gray()),
                Line::from(vec![
                    " > ".bold().cyan(),
                    app.palette_query.as_str().into(),
                    "▊".cyan(), // cursor block
                ]),
            ];

            let w = 45.min(chunks[0].width.saturating_sub(1));
            let h = 4.min(chunks[0].height.saturating_sub(1));

            if app.palette_height != h {
                app.palette_height = h;
                app.needs_clear_once = true;
            }

            let palette_block = Block::default()
                .title(prompt_title)
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Cyan))
                .title_style(Style::default().fg(Color::Yellow).bold());

            let palette_paragraph = Paragraph::new(lines)
                .block(palette_block)
                .style(Style::default().fg(Color::White).bg(Color::Reset));

            let x = chunks[0].x + chunks[0].width.saturating_sub(w).saturating_sub(1);
            let y = chunks[0].y.saturating_add(1);

            let popup_area = Rect::new(x, y, w, h);
            frame.render_widget(Clear, popup_area);
            frame.render_widget(palette_paragraph, popup_area);
        } else {
            let title = match app.palette_mode {
                PaletteMode::File => " File Search ",
                PaletteMode::Command => " Command Palette ",
                _ => "",
            };

            // Determine dynamic visible_count and palette_height
            let total_items = match app.palette_mode {
                PaletteMode::File => app.get_filtered_files().len(),
                PaletteMode::Command => app.get_filtered_commands().len(),
                _ => 0,
            };
            let max_height = (chunks[0].height as f64 * 0.5).round() as u16;
            let mut palette_h = (total_items as u16 + 4).max(12);
            palette_h = palette_h.min(max_height);

            if app.palette_height != palette_h {
                app.palette_height = palette_h;
                app.needs_clear_once = true;
            }

            let visible_count = (app.palette_height as usize).saturating_sub(4);
            let palette_height = app.palette_height;

            let mut lines = vec![
                Line::from(vec![
                    " > ".bold().cyan(),
                    app.palette_query.clone().into(),
                    "▊".cyan(), // cursor block
                ]),
                Line::from("──────────────────────────────────────────────────────────".gray()),
            ];

            match app.palette_mode {
                PaletteMode::File => {
                    let filtered_files = app.get_filtered_files();
                    if !filtered_files.is_empty() {
                        app.palette_selected_index =
                            app.palette_selected_index.min(filtered_files.len() - 1);
                    } else {
                        app.palette_selected_index = 0;
                    }

                    let total_files = filtered_files.len();
                    let half_visible = visible_count / 2;
                    let start_idx = if total_files <= visible_count
                        || app.palette_selected_index < half_visible
                    {
                        0
                    } else if app.palette_selected_index >= total_files.saturating_sub(half_visible)
                    {
                        total_files.saturating_sub(visible_count)
                    } else {
                        app.palette_selected_index.saturating_sub(half_visible)
                    };

                    for (i, (_, filename)) in filtered_files
                        .iter()
                        .enumerate()
                        .skip(start_idx)
                        .take(visible_count)
                    {
                        let mut line = Line::from(format!("   {}", filename));
                        if i == app.palette_selected_index {
                            line = Line::from(format!(" > {}", filename))
                                .bold()
                                .yellow()
                                .on_blue();
                        }
                        lines.push(line);
                    }

                    if filtered_files.is_empty() {
                        lines.push(Line::from("   No matches found.".gray().italic()));
                    }
                }
                PaletteMode::Command => {
                    let filtered_commands = app.get_filtered_commands();
                    if !filtered_commands.is_empty() {
                        app.palette_selected_index =
                            app.palette_selected_index.min(filtered_commands.len() - 1);
                    } else {
                        app.palette_selected_index = 0;
                    }

                    let total_cmds = filtered_commands.len();
                    let half_visible = visible_count / 2;
                    let start_idx = if total_cmds <= visible_count
                        || app.palette_selected_index < half_visible
                    {
                        0
                    } else if app.palette_selected_index >= total_cmds.saturating_sub(half_visible)
                    {
                        total_cmds.saturating_sub(visible_count)
                    } else {
                        app.palette_selected_index.saturating_sub(half_visible)
                    };

                    for (i, cmd) in filtered_commands
                        .iter()
                        .enumerate()
                        .skip(start_idx)
                        .take(visible_count)
                    {
                        let cmd_line = vec![
                            if i == app.palette_selected_index {
                                " > "
                            } else {
                                "   "
                            }
                            .into(),
                            cmd.item.name.bold(),
                            " - ".into(),
                            cmd.item.description.gray(),
                        ];
                        let mut line = Line::from(cmd_line);
                        if i == app.palette_selected_index {
                            line = line.yellow().on_blue();
                        }
                        lines.push(line);
                    }

                    if filtered_commands.is_empty() {
                        lines.push(Line::from("   No matches found.".gray().italic()));
                    }
                }
                _ => {}
            }

            let mut palette_width = app.palette_width;
            let cap_width = (chunks[0].width as f64 * 0.75).round() as u16;
            palette_width = palette_width.max(40).min(cap_width);

            if lines.len() > 1 {
                let inner_w = palette_width.saturating_sub(2) as usize;
                lines[1] = Line::from("─".repeat(inner_w).gray());
            }

            let palette_block = Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Cyan))
                .title_style(Style::default().fg(Color::Yellow).bold());

            let palette_paragraph = Paragraph::new(lines)
                .block(palette_block)
                .style(Style::default().fg(Color::White).bg(Color::Reset));

            let w = palette_width.min(chunks[0].width.saturating_sub(1));
            let h = palette_height.min(chunks[0].height.saturating_sub(1));
            let x = chunks[0].x + chunks[0].width.saturating_sub(w).saturating_sub(1);
            let y = chunks[0].y.saturating_add(1);

            let popup_area = Rect::new(x, y, w, h);

            frame.render_widget(Clear, popup_area);
            frame.render_widget(palette_paragraph, popup_area);
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Check if we have piped input via stdin (e.g. from fd or find)
    let mut piped_files = Vec::new();
    let is_piped = !io::stdin().is_terminal();
    if is_piped {
        use std::io::BufRead;
        let stdin = io::stdin();
        for line in stdin.lock().lines().map_while(Result::ok) {
            let path = PathBuf::from(line.trim());
            if path.exists() && path.is_file() {
                piped_files.push(path);
            }
        }

        // Reopen stdin from /dev/tty so crossterm can read keyboard inputs!
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            if let Ok(tty) = std::fs::OpenOptions::new().read(true).open("/dev/tty") {
                let fd = tty.as_raw_fd();
                unsafe {
                    libc::dup2(fd, libc::STDIN_FILENO);
                }
            }
        }
    }

    // Parse CLI arguments
    let args: Vec<String> = env::args().collect();
    let mut initial_path = None;
    let mut filter_opt = None;
    let mut protocol_opt = None;
    let mut scale_opt = None;
    let mut slideshow_opt = None;
    let mut check_magic = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--filter" | "-f" => {
                if i + 1 < args.len() {
                    filter_opt = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    eprintln!(
                        "Error: --filter / -f requires an argument (nearest, linear, cubic, mitchell, gaussian, lanczos, hamming)"
                    );
                    std::process::exit(1);
                }
            }
            "--protocol" | "-p" => {
                if i + 1 < args.len() {
                    protocol_opt = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    eprintln!(
                        "Error: --protocol / -p requires an argument (kitty, sixel, halfblocks, iterm2)"
                    );
                    std::process::exit(1);
                }
            }
            "--scale" | "-s" => {
                if i + 1 < args.len() {
                    scale_opt = Some(args[i + 1].clone());
                    i += 2;
                } else {
                    eprintln!(
                        "Error: --scale / -s requires an argument (none, actual, shrink, full, crop)"
                    );
                    std::process::exit(1);
                }
            }
            "--slideshow" | "-t" => {
                if i + 1 < args.len() {
                    if let Ok(sec) = args[i + 1].parse::<u32>() {
                        slideshow_opt = Some(sec);
                    } else {
                        eprintln!("Error: -t / --slideshow requires a positive integer argument");
                        std::process::exit(1);
                    }
                    i += 2;
                } else {
                    eprintln!("Error: -t / --slideshow requires an argument");
                    std::process::exit(1);
                }
            }
            "--check-magic" | "-m" => {
                check_magic = true;
                i += 1;
            }
            "--help" | "-h" => {
                println!("imv-tui: A fast keyboard-driven terminal image viewer");
                println!();
                println!("Usage: imv-tui [path] [options]");
                println!();
                println!("Options:");
                println!(
                    "  -f, --filter <filter>      Initial image scaling filter: nearest, linear, cubic, mitchell, gaussian, lanczos, hamming"
                );
                println!(
                    "  -p, --protocol <protocol>  Force terminal graphics protocol: kitty, sixel, halfblocks, iterm2"
                );
                println!(
                    "  -s, --scale <mode>         Initial image scaling mode: none, actual, shrink, full, crop (defaults to shrink)"
                );
                println!(
                    "  -t, --slideshow <seconds>  Start the slideshow with the given delay in seconds"
                );
                println!(
                    "  -m, --check-magic          Check file magic bytes on startup (slower on network drives)"
                );
                println!("  -h, --help                 Show this help menu");
                std::process::exit(0);
            }
            val => {
                if initial_path.is_none() {
                    initial_path = Some(PathBuf::from(val));
                }
                i += 1;
            }
        }
    }

    // Get the image file list and current starting index
    let (images, current_index) = if is_piped && !piped_files.is_empty() {
        let sources = collect_sources(&piped_files, check_magic)?;
        (sources, 0)
    } else {
        let initial_path = initial_path.unwrap_or_else(|| PathBuf::from("."));
        if is_cbz_or_zip(&initial_path, check_magic) {
            let pages = list_cbz_pages(&initial_path)?;
            let sources = pages
                .into_iter()
                .map(|page| ImageSource::Cbz {
                    zip_path: initial_path.clone(),
                    file_in_zip: page,
                })
                .collect();
            (sources, 0)
        } else {
            let (paths, index) = scan_directory(&initial_path, check_magic)?;
            let sources = paths.into_iter().map(ImageSource::Local).collect();
            (sources, index)
        }
    };

    let initial_filter = match filter_opt.as_deref() {
        Some("nearest") => FilterType::Nearest,
        Some("linear") => FilterType::Triangle,
        Some("cubic") => FilterType::CatmullRom,
        Some("mitchell") => FilterType::Mitchell,
        Some("gaussian") => FilterType::Gaussian,
        Some("lanczos") => FilterType::Lanczos3,
        Some("hamming") => FilterType::Hamming,
        Some(other) => {
            eprintln!(
                "Error: Unknown filter '{}'. Choose from: nearest, linear, cubic, mitchell, gaussian, lanczos, hamming",
                other
            );
            std::process::exit(1);
        }
        None => FilterType::Nearest,
    };

    let scale_mode = match scale_opt.as_deref() {
        Some("none") | Some("actual") => ScaleMode::None,
        Some("shrink") => ScaleMode::Shrink,
        Some("full") | Some("fit") => ScaleMode::Full,
        Some("crop") => ScaleMode::Crop,
        Some(other) => {
            eprintln!(
                "Error: Unknown scale mode '{}'. Choose from: none, actual, shrink, full, crop",
                other
            );
            std::process::exit(1);
        }
        None => ScaleMode::Shrink,
    };

    // Query terminal protocol before raw mode
    let mut picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    if let Some(proto_str) = protocol_opt.as_deref() {
        let proto = match proto_str.to_lowercase().as_str() {
            "kitty" => ProtocolType::Kitty,
            "sixel" => ProtocolType::Sixel,
            "halfblocks" | "halfblock" => ProtocolType::Halfblocks,
            "iterm2" => ProtocolType::Iterm2,
            other => {
                eprintln!(
                    "Error: Unknown protocol '{}'. Choose from: kitty, sixel, halfblocks, iterm2",
                    other
                );
                std::process::exit(1);
            }
        };
        picker.set_protocol_type(proto);
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app
    let mut app = match App::new(images, current_index, picker, initial_filter, scale_mode) {
        Ok(app) => app,
        Err(e) => {
            // Restore terminal on init error
            disable_raw_mode()?;
            execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture)?;
            eprintln!("Initialization Error: {}", e);
            std::process::exit(1);
        }
    };
    if let Some(sec) = slideshow_opt {
        app.slideshow_seconds = sec;
        app.slideshow_last_transition = std::time::Instant::now();
    }

    // Main event loop
    while app.running {
        app.update_channels();

        // Automatic slideshow transition
        if app.slideshow_seconds > 0
            && !app.is_loading
            && app.slideshow_last_transition.elapsed()
                >= std::time::Duration::from_secs(app.slideshow_seconds as u64)
        {
            app.next_image();
            app.slideshow_last_transition = std::time::Instant::now();
        }

        if app.needs_clear_once {
            app.needs_clear_once = false;
            terminal.clear()?;
        }

        if app.needs_clear {
            app.needs_clear = false;
            if app.should_clear_on_update() {
                terminal.clear()?;
            }
        }
        terminal.draw(|f| ui(f, &mut app))?;

        if event::poll(Duration::from_millis(50))? {
            let mut events = Vec::new();
            events.push(event::read()?);
            while event::poll(Duration::from_millis(0))? {
                events.push(event::read()?);
            }

            for ev in events {
                match ev {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        if app.palette_mode != PaletteMode::Closed {
                            match key.code {
                                KeyCode::Esc => {
                                    app.palette_mode = PaletteMode::Closed;
                                    app.needs_update = true;
                                    app.needs_clear_once = true;
                                }
                                KeyCode::Enter => match app.palette_mode {
                                    PaletteMode::File => {
                                        let files = app.get_filtered_files();
                                        if !files.is_empty()
                                            && app.palette_selected_index < files.len()
                                        {
                                            app.current_index = files[app.palette_selected_index].0;
                                            app.start_load_image();
                                        }
                                        app.palette_mode = PaletteMode::Closed;
                                        app.needs_update = true;
                                        app.needs_clear_once = true;
                                    }
                                    PaletteMode::Command => {
                                        let cmds = app.get_filtered_commands();
                                        if !cmds.is_empty()
                                            && app.palette_selected_index < cmds.len()
                                        {
                                            let cmd = cmds[app.palette_selected_index].cmd;
                                            app.execute_command(cmd);
                                        }
                                        if app.palette_mode == PaletteMode::Command {
                                            app.palette_mode = PaletteMode::Closed;
                                            app.needs_update = true;
                                            app.needs_clear_once = true;
                                        }
                                    }
                                    PaletteMode::Prompt => {
                                        if let Some(prompt_type) = app.prompt_type {
                                            app.execute_prompt(prompt_type);
                                        }
                                    }
                                    _ => {}
                                },
                                KeyCode::Up if app.palette_selected_index > 0 => {
                                    app.palette_selected_index -= 1;
                                }
                                KeyCode::Down => {
                                    let max_len = match app.palette_mode {
                                        PaletteMode::File => app.get_filtered_files().len(),
                                        PaletteMode::Command => app.get_filtered_commands().len(),
                                        _ => 0,
                                    };
                                    if max_len > 0 && app.palette_selected_index < max_len - 1 {
                                        app.palette_selected_index += 1;
                                    }
                                }
                                KeyCode::PageUp => {
                                    let max_len = match app.palette_mode {
                                        PaletteMode::File => app.get_filtered_files().len(),
                                        PaletteMode::Command => app.get_filtered_commands().len(),
                                        _ => 0,
                                    };
                                    let term_size = terminal.size().unwrap_or_default();
                                    let viewport_h = term_size.height.saturating_sub(1);
                                    let max_h = (viewport_h as f64 * 0.5).round() as u16;
                                    let palette_h = (max_len as u16 + 4).max(12).min(max_h);
                                    let page_size = (palette_h as usize).saturating_sub(4);

                                    app.palette_selected_index =
                                        app.palette_selected_index.saturating_sub(page_size);
                                }
                                KeyCode::PageDown => {
                                    let max_len = match app.palette_mode {
                                        PaletteMode::File => app.get_filtered_files().len(),
                                        PaletteMode::Command => app.get_filtered_commands().len(),
                                        _ => 0,
                                    };
                                    if max_len > 0 {
                                        let term_size = terminal.size().unwrap_or_default();
                                        let viewport_h = term_size.height.saturating_sub(1);
                                        let max_h = (viewport_h as f64 * 0.5).round() as u16;
                                        let palette_h = (max_len as u16 + 4).max(12).min(max_h);
                                        let page_size = (palette_h as usize).saturating_sub(4);

                                        app.palette_selected_index = (app.palette_selected_index
                                            + page_size)
                                            .min(max_len - 1);
                                    }
                                }
                                KeyCode::Char('k')
                                    if key.modifiers.contains(event::KeyModifiers::CONTROL)
                                        && app.palette_selected_index > 0 =>
                                {
                                    app.palette_selected_index -= 1;
                                }
                                KeyCode::Char('j')
                                    if key.modifiers.contains(event::KeyModifiers::CONTROL) =>
                                {
                                    let max_len = match app.palette_mode {
                                        PaletteMode::File => app.get_filtered_files().len(),
                                        PaletteMode::Command => app.get_filtered_commands().len(),
                                        _ => 0,
                                    };
                                    if max_len > 0 && app.palette_selected_index < max_len - 1 {
                                        app.palette_selected_index += 1;
                                    }
                                }
                                KeyCode::Backspace => {
                                    app.palette_query.pop();
                                    app.palette_selected_index = 0;
                                }
                                KeyCode::Char(c) => {
                                    app.palette_query.push(c);
                                    app.palette_selected_index = 0;
                                }
                                _ => {}
                            }
                        } else {
                            let mut key_handled = false;
                            if app.show_help {
                                match key.code {
                                    KeyCode::Char('?') | KeyCode::Char('/') => {
                                        app.toggle_help();
                                        key_handled = true;
                                    }
                                    KeyCode::Char('q') | KeyCode::Esc => {
                                        app.show_help = false;
                                        app.needs_update = true;
                                        app.needs_clear_once = true;
                                        key_handled = true;
                                    }
                                    _ => {
                                        app.show_help = false;
                                        app.needs_update = true;
                                        app.needs_clear_once = true;
                                    }
                                }
                            }

                            if !key_handled && let Some(cmd) = Command::from_key(key) {
                                app.execute_command(cmd);
                            }
                        }
                    }
                    Event::Mouse(mouse_event) => {
                        if app.show_help {
                            app.show_help = false;
                            app.needs_update = true;
                            app.needs_clear_once = true;
                        }
                        match mouse_event.kind {
                            MouseEventKind::ScrollUp => {
                                app.execute_command(Command::ZoomIn);
                            }
                            MouseEventKind::ScrollDown => {
                                app.execute_command(Command::ZoomOut);
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    // Cleanup terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}
