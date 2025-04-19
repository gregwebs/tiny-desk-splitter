use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

pub fn overwrite_dir<P: AsRef<Path>>(path: P) -> Result<()> {
    let path_ref = path.as_ref();
    let path_str = path_ref.to_string_lossy();

    if fs::exists(&path)
        .with_context(|| format!("Failed to check if directory exists: {}", path_str))?
    {
        fs::remove_dir_all(&path)
            .with_context(|| format!("Failed to remove existing directory: {}", path_str))?;
    }

    fs::create_dir(&path).with_context(|| format!("Failed to create directory: {}", path_str))
}

pub fn sanitize_filename(input: &str) -> String {
    // Replace characters that are problematic in filenames
    let mut sanitized = input.replace(
        &['/', '\\', ':', '*', '?', '"', '<', '>', '|', '\0'][..],
        "_",
    ).replace("__", "_");

    // Trim leading/trailing whitespace and dots
    sanitized = sanitized.trim().trim_matches('.').to_string();

    // If the name is empty after sanitization, provide a default
    if sanitized.is_empty() {
        sanitized = "untitled".to_string();
    }

    sanitized
}
