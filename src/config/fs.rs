use crate::error::HackArenaError;
use std::fs;
use std::path::Path;

/// Ensures that a directory exists (creates it recursively).
pub fn ensure_dir(path: &Path) -> Result<(), HackArenaError> {
    fs::create_dir_all(path).map_err(|e| HackArenaError::io_with_path(path, e))
}
