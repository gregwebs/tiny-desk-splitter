use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

pub fn ensure_dir<P: AsRef<Path>>(path: P) -> Result<()> {
    let path_ref = path.as_ref();
    let path_str = path_ref.to_string_lossy();

    // Single create + tolerate AlreadyExists, rather than check-then-create:
    // the latter is a TOCTOU race when two callers `ensure_dir` the same path
    // concurrently (e.g. parallel tests sharing a scratch directory name) —
    // both see "missing", both call create_dir, the loser gets ErrorKind::
    // AlreadyExists / os error 17.
    match fs::create_dir(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("Failed to create directory: {}", path_str))
        }
    }
}

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
    let mut sanitized = input
        .replace(
            &['/', '\\', ':', '*', '?', '"', '<', '>', '|', '\0'][..],
            "_",
        )
        .replace("__", "_");

    // Trim leading/trailing whitespace and dots
    sanitized = sanitized.trim().trim_matches('.').to_string();

    // If the name is empty after sanitization, provide a default
    if sanitized.is_empty() {
        sanitized = "untitled".to_string();
    }

    sanitized
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_dir_is_idempotent_under_concurrent_calls() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("shared");

        let handles: Vec<_> = (0..16)
            .map(|_| {
                let target = target.clone();
                std::thread::spawn(move || ensure_dir(&target))
            })
            .collect();

        for handle in handles {
            handle.join().unwrap().unwrap();
        }
        assert!(target.is_dir());
    }
}
