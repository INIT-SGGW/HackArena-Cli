use crate::constants::{PROJECT_CONFIG_FILE, PROJECT_MANIFEST_FILE, PROJECT_META_DIR};
use crate::error::HackArenaError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Project configuration stored in `./.hackarena/project.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub edition: String,
    #[serde(default)]
    pub wrapper_id: Option<String>,
    pub backend_dir: PathBuf,
}

/// Returns the `.hackarena` directory path for a project root.
pub fn project_meta_dir(project_root: &Path) -> PathBuf {
    project_root.join(PROJECT_META_DIR)
}

/// Returns `./.hackarena/project.json` for a project root.
pub fn project_config_path(project_root: &Path) -> PathBuf {
    project_meta_dir(project_root).join(PROJECT_CONFIG_FILE)
}

/// Returns `./.hackarena/manifest.json` for a project root.
pub fn project_manifest_path(project_root: &Path) -> PathBuf {
    project_meta_dir(project_root).join(PROJECT_MANIFEST_FILE)
}

/// Loads `./.hackarena/project.json`.
pub fn load_project_config(project_root: &Path) -> Result<ProjectConfig, HackArenaError> {
    let path = project_config_path(project_root);
    let bytes = fs::read(&path).map_err(|e| HackArenaError::io_with_path(&path, e))?;
    Ok(serde_json::from_slice(&bytes).map_err(|e| HackArenaError::json_with_path(&path, e))?)
}

/// Writes `./.hackarena/project.json` (creates parent directories).
pub fn save_project_config(
    project_root: &Path,
    config: &ProjectConfig,
) -> Result<(), HackArenaError> {
    let meta_dir = project_meta_dir(project_root);
    fs::create_dir_all(&meta_dir).map_err(|e| HackArenaError::io_with_path(&meta_dir, e))?;
    let path = project_config_path(project_root);
    let data =
        serde_json::to_vec_pretty(config).map_err(|e| HackArenaError::json_with_path(&path, e))?;
    fs::write(&path, data).map_err(|e| HackArenaError::io_with_path(&path, e))?;
    Ok(())
}
