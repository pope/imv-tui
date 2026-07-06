use crate::imaging::ImageSource;

pub struct ImageQueue {
    /// Loaded list of image sources.
    pub images: Vec<ImageSource>,
    /// Common prefix stripped relative paths for search.
    pub relative_paths: Vec<String>,
    /// Lowercase relative paths for case-insensitive matching.
    pub relative_paths_lowercase: Vec<String>,
    /// Current selected index in the images vector.
    pub current_index: usize,
}

impl ImageQueue {
    /// Creates a new ImageQueue, returning an error if the images list is empty.
    pub fn new(images: Vec<ImageSource>, current_index: usize) -> Result<Self, String> {
        if images.is_empty() {
            return Err("No supported images found".to_string());
        }

        // Calculate common prefix path among parent directories of all image sources
        let mut first_path = match &images[0] {
            ImageSource::Local(p) => p.parent().unwrap_or(p).to_path_buf(),
            ImageSource::Cbz { zip_path, .. } => {
                zip_path.parent().unwrap_or(zip_path).to_path_buf()
            }
        };
        for img in &images[1..] {
            let p = match img {
                ImageSource::Local(p) => p.parent().unwrap_or(p).to_path_buf(),
                ImageSource::Cbz { zip_path, .. } => {
                    zip_path.parent().unwrap_or(zip_path).to_path_buf()
                }
            };
            while !p.starts_with(&first_path) {
                if !first_path.pop() {
                    break;
                }
            }
        }
        let common_prefix = first_path;

        // Generate relative paths
        let relative_paths: Vec<String> = images
            .iter()
            .map(|img| match img {
                ImageSource::Local(path) => {
                    if let Ok(rel) = path.strip_prefix(&common_prefix) {
                        rel.to_string_lossy().to_string()
                    } else {
                        path.to_string_lossy().to_string()
                    }
                }
                ImageSource::Cbz {
                    zip_path,
                    file_in_zip,
                } => {
                    if let Ok(rel_zip) = zip_path.strip_prefix(&common_prefix) {
                        format!("{}: {}", rel_zip.to_string_lossy(), file_in_zip)
                    } else {
                        let zip_name = zip_path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("Unknown");
                        format!("{}: {}", zip_name, file_in_zip)
                    }
                }
            })
            .collect();

        let relative_paths_lowercase: Vec<String> = relative_paths
            .iter()
            .map(|name| name.to_lowercase())
            .collect();

        let current_index = current_index.min(images.len() - 1);
        Ok(Self {
            images,
            relative_paths,
            relative_paths_lowercase,
            current_index,
        })
    }

    /// Returns true if the image queue contains no images.
    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_common_prefix_stripping() {
        let images = vec![
            ImageSource::Local(PathBuf::from("/home/user/Pictures/2026/vacation/lake.png")),
            ImageSource::Local(PathBuf::from(
                "/home/user/Pictures/2026/vacation/mountains.png",
            )),
            ImageSource::Local(PathBuf::from("/home/user/Pictures/2026/work/office.png")),
        ];
        let queue = ImageQueue::new(images, 0).unwrap();

        assert_eq!(queue.relative_paths[0], "vacation/lake.png");
        assert_eq!(queue.relative_paths[1], "vacation/mountains.png");
        assert_eq!(queue.relative_paths[2], "work/office.png");
    }
}
