use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use crate::app::adjustments::ImageAdjustments;
use crate::imaging::ImageSource;
use crate::imaging::types::{Brightness, Contrast, Rotation};

/// The classification/flagged state for an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Classification {
    Unflagged,
    Pick,
    Reject,
}

impl Classification {
    pub fn icon(&self) -> &'static str {
        match self {
            Self::Unflagged => "⚪",
            Self::Pick => "⭐",
            Self::Reject => "❌",
        }
    }

    pub fn display_label(&self) -> &'static str {
        match self {
            Self::Unflagged => "⚪ Unflagged",
            Self::Pick => "⭐ Pick",
            Self::Reject => "❌ Reject",
        }
    }

    pub fn search_prefix(&self) -> &'static str {
        match self {
            Self::Unflagged => "   ",
            Self::Pick => "⭐ ",
            Self::Reject => "❌ ",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViewMode {
    Default,
    Unflagged,
    Picks,
    Rejects,
    All,
}

impl ViewMode {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Default => "Unflagged + Picks",
            Self::Unflagged => "Unflagged Only",
            Self::Picks => "Picks Only",
            Self::Rejects => "Rejects Only",
            Self::All => "All Files",
        }
    }
}

/// Helper struct for JSON serialization/deserialization of classifications.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassificationJsonItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive: Option<String>,
    pub filename: String,
    pub flag: String,

    #[serde(default, skip_serializing_if = "Brightness::is_zero")]
    pub brightness: Brightness,
    #[serde(default, skip_serializing_if = "Contrast::is_zero")]
    pub contrast: Contrast,
    #[serde(default, skip_serializing_if = "Rotation::is_zero")]
    pub rotation: Rotation,
}

pub fn is_image_visible(
    index: usize,
    classifications: &[Classification],
    view_mode: ViewMode,
) -> bool {
    let classification = classifications
        .get(index)
        .copied()
        .unwrap_or(Classification::Unflagged);
    match view_mode {
        ViewMode::Default => {
            classification == Classification::Pick || classification == Classification::Unflagged
        }
        ViewMode::Unflagged => classification == Classification::Unflagged,
        ViewMode::Picks => classification == Classification::Pick,
        ViewMode::Rejects => classification == Classification::Reject,
        ViewMode::All => true,
    }
}

pub fn import_from_file(
    import_path: &Path,
) -> Result<HashMap<String, (Classification, ImageAdjustments)>, String> {
    let content = std::fs::read_to_string(import_path)
        .map_err(|e| format!("Failed to read import file: {}", e))?;

    let mut imported: HashMap<String, (Classification, ImageAdjustments)> = HashMap::new();

    if import_path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
    {
        let parsed: Vec<ClassificationJsonItem> = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse JSON manifest: {}", e))?;
        for item in parsed {
            let class = match item.flag.to_lowercase().as_str() {
                "pick" | "picked" => Classification::Pick,
                "reject" | "rejected" => Classification::Reject,
                _ => Classification::Unflagged,
            };
            let adj = ImageAdjustments {
                brightness: item.brightness,
                contrast: item.contrast,
                rotation: item.rotation,
            };
            let key = if let Some(ref archive_path) = item.archive {
                format!("{}::{}", archive_path, item.filename)
            } else {
                item.filename
            };
            imported.insert(key, (class, adj));
        }
    } else {
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let split_res = line.split_once('\t').or_else(|| line.split_once(':'));
            if let Some((prefix, ident)) = split_res {
                let ident = ident.trim().to_string();
                let class = match prefix.trim().to_uppercase().as_str() {
                    "PICK" | "PICKED" => Classification::Pick,
                    "REJECT" | "REJECTED" => Classification::Reject,
                    "UNFLAGGED" => Classification::Unflagged,
                    _ => continue,
                };
                imported.insert(ident, (class, ImageAdjustments::default()));
            }
        }
    }

    Ok(imported)
}

pub fn export_to_file(
    export_path: &Path,
    images: &[ImageSource],
    classifications: &[Classification],
    adjustments: &[ImageAdjustments],
) -> Result<(), String> {
    let mut text_lines = Vec::new();
    let mut json_items = Vec::new();

    for (idx, img) in images.iter().enumerate() {
        let class = classifications
            .get(idx)
            .cloned()
            .unwrap_or(Classification::Unflagged);
        let adj = adjustments.get(idx).cloned().unwrap_or_default();

        // Only export if image is flagged OR has non-default adjustments
        if class == Classification::Unflagged && adj == ImageAdjustments::default() {
            continue;
        }

        let ident = img.identifier();

        let (archive, filename) = match img {
            ImageSource::Local(path) => {
                let abs = if path.is_absolute() {
                    path.clone()
                } else if let Ok(curr) = std::env::current_dir() {
                    curr.join(path)
                } else {
                    path.clone()
                };
                (None, abs.to_string_lossy().into_owned())
            }
            ImageSource::Cbz {
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
                (
                    Some(abs_zip.to_string_lossy().into_owned()),
                    file_in_zip.clone(),
                )
            }
        };

        let flag_str = match class {
            Classification::Pick => "picked",
            Classification::Reject => "rejected",
            Classification::Unflagged => "unflagged",
        };

        json_items.push(ClassificationJsonItem {
            archive,
            filename,
            flag: flag_str.to_string(),
            brightness: adj.brightness,
            contrast: adj.contrast,
            rotation: adj.rotation,
        });

        // For text export: only write if flagged
        if class != Classification::Unflagged {
            let text_state = match class {
                Classification::Pick => "PICK",
                Classification::Reject => "REJECT",
                Classification::Unflagged => "UNFLAGGED",
            };
            text_lines.push(format!("{}\t{}", text_state, ident));
        }
    }

    if export_path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
    {
        let json_str = serde_json::to_string_pretty(&json_items)
            .map_err(|e| format!("Failed to serialize classifications: {}", e))?;
        std::fs::write(export_path, json_str)
            .map_err(|e| format!("Failed to write export JSON file: {}", e))?;
    } else {
        std::fs::write(export_path, text_lines.join("\n"))
            .map_err(|e| format!("Failed to write export text file: {}", e))?;
    }

    Ok(())
}
