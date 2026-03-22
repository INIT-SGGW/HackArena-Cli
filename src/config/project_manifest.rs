use crate::error::HackArenaError;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Project-local manifest stored in `./.hackarena/manifest.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectManifest {
    #[serde(default)]
    pub auth: Option<ProjectInstalledBinary>,
    #[serde(default)]
    pub backend: Option<ProjectInstalledBundle>,
    #[serde(default)]
    pub wrappers: BTreeMap<String, ProjectInstalledBundle>,

    // Backward-compat for older manifests that stored a single wrapper.
    #[serde(default, rename = "wrapper")]
    pub wrapper_legacy: Option<ProjectInstalledBundle>,
}

/// A globally installed binary referenced from a project manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectInstalledBinary {
    pub path: PathBuf,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub installed_at_unix: Option<u64>,
}

/// An installed bundle (backend/wrapper) referenced from a project manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectInstalledBundle {
    pub url: String,
    pub install_dir: PathBuf,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub installed_at_unix: Option<u64>,
    #[serde(default)]
    pub files: Vec<PathBuf>,
}

/// Loads `./.hackarena/manifest.json`; returns default if missing.
pub fn load_project_manifest(project_root: &Path) -> Result<ProjectManifest, HackArenaError> {
    let path = crate::config::project_manifest_path(project_root);
    if !path.exists() {
        return Ok(ProjectManifest::default());
    }
    let bytes = fs::read(&path).map_err(|e| HackArenaError::io_with_path(&path, e))?;
    let mut m: ProjectManifest =
        serde_json::from_slice(&bytes).map_err(|e| HackArenaError::json_with_path(&path, e))?;
    if m.wrappers.is_empty() {
        if let Some(w) = m.wrapper_legacy.take() {
            if let Some(id) = w
                .install_dir
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
            {
                m.wrappers.insert(id, w);
            }
        }
    }
    Ok(m)
}

/// Writes `./.hackarena/manifest.json` (creates parent directories).
pub fn save_project_manifest(
    project_root: &Path,
    manifest: &ProjectManifest,
) -> Result<(), HackArenaError> {
    let meta_dir = crate::config::project_meta_dir(project_root);
    fs::create_dir_all(&meta_dir).map_err(|e| HackArenaError::io_with_path(&meta_dir, e))?;
    let path = crate::config::project_manifest_path(project_root);
    let data = serde_json::to_vec_pretty(manifest)
        .map_err(|e| HackArenaError::json_with_path(&path, e))?;
    fs::write(&path, data).map_err(|e| HackArenaError::io_with_path(&path, e))?;
    Ok(())
}
