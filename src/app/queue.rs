use crate::imaging::ImageSource;

pub struct ImageQueue {
    /// Loaded list of image sources.
    pub images: Vec<ImageSource>,
    /// Pre-computed filename cache for standard status display.
    pub display_names: Vec<String>,
    /// Lowercase file name cache for case-insensitive matching.
    pub display_names_lowercase: Vec<String>,
    /// Current selected index in the images vector.
    pub current_index: usize,
}

impl ImageQueue {
    /// Creates a new ImageQueue, returning an error if the images list is empty.
    pub fn new(images: Vec<ImageSource>, current_index: usize) -> Result<Self, String> {
        if images.is_empty() {
            return Err("No supported images found".to_string());
        }
        let display_names: Vec<String> = images.iter().map(|img| img.display_name()).collect();
        let display_names_lowercase: Vec<String> = display_names
            .iter()
            .map(|name| name.to_lowercase())
            .collect();
        let current_index = current_index.min(images.len() - 1);
        Ok(Self {
            images,
            display_names,
            display_names_lowercase,
            current_index,
        })
    }

    /// Returns true if the image queue contains no images.
    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    /// Returns the filename display name of the currently selected image.
    pub fn get_current_filename(&self) -> &str {
        self.display_names
            .get(self.current_index)
            .map(|s| s.as_str())
            .unwrap_or("")
    }
}
