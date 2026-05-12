use crate::config::workspace::{
    PROJECT_LAYOUT_VERSION_V2, ProjectContext, WorkspaceControl, save_workspace_control,
    workspace_root_for_edition,
};
use crate::config::{
    ensure_dir, load_project_config, project_config_path, project_manifest_path, project_meta_dir,
};
use crate::constants::{PROJECT_STANDALONE_DIR, PROJECT_WRAPPERS_DIR};
use crate::error::HackArenaError;
use std::fs;
use std::path::Path;

pub(crate) fn migrate_legacy_root_layout(
    repo_root: &Path,
) -> Result<ProjectContext, HackArenaError> {
    let project = load_project_config(repo_root)?;
    let workspace_root = workspace_root_for_edition(repo_root, &project.edition);
    if workspace_root.exists() {
        return Err(HackArenaError::msg(format!(
            "Cannot migrate legacy project layout: destination workspace already exists at {}.",
            workspace_root.display()
        )));
    }

    eprintln!(
        "Migrating legacy project layout into {}...",
        workspace_root.display()
    );

    ensure_dir(&project_meta_dir(&workspace_root))?;

    move_if_exists(
        &project_config_path(repo_root),
        &project_config_path(&workspace_root),
    )?;
    move_if_exists(
        &project_manifest_path(repo_root),
        &project_manifest_path(&workspace_root),
    )?;
    move_if_exists(
        &repo_root.join(&project.backend_dir),
        &workspace_root.join(&project.backend_dir),
    )?;
    move_if_exists(
        &repo_root.join(PROJECT_STANDALONE_DIR),
        &workspace_root.join(PROJECT_STANDALONE_DIR),
    )?;
    move_if_exists(
        &repo_root.join(PROJECT_WRAPPERS_DIR),
        &workspace_root.join(PROJECT_WRAPPERS_DIR),
    )?;

    save_workspace_control(
        repo_root,
        &WorkspaceControl {
            layout_version: PROJECT_LAYOUT_VERSION_V2,
            active_edition: project.edition.clone(),
        },
    )?;

    eprintln!("Project workspace migration complete.");

    Ok(ProjectContext {
        repo_root: repo_root.to_path_buf(),
        workspace_root,
    })
}

fn move_if_exists(src: &Path, dest: &Path) -> Result<(), HackArenaError> {
    if !src.exists() {
        return Ok(());
    }
    if dest.exists() {
        return Err(HackArenaError::msg(format!(
            "Cannot migrate legacy project layout: destination already exists at {}.",
            dest.display()
        )));
    }
    if let Some(parent) = dest.parent() {
        ensure_dir(parent)?;
    }
    fs::rename(src, dest).map_err(|e| HackArenaError::io_with_path(dest, e))?;
    Ok(())
}
