use crate::config::workspace::{
    PROJECT_LAYOUT_VERSION_V2, ProjectContext, WorkspaceControl, editions_root,
    save_workspace_control, workspace_root_for_edition, workspace_roots_with_project_config,
};
use crate::config::{
    ProjectConfig, ensure_dir, load_project_config, project_config_path, project_manifest_path,
    project_meta_dir,
};
use crate::constants::{PROJECT_STANDALONE_DIR, PROJECT_WRAPPERS_DIR};
use crate::error::HackArenaError;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub(crate) struct LegacyProjectState {
    pub project: ProjectConfig,
    pub workspace_root: PathBuf,
    pub is_partial: bool,
}

pub(crate) fn detect_legacy_project_state(
    repo_root: &Path,
) -> Result<Option<LegacyProjectState>, HackArenaError> {
    let root_project_path = project_config_path(repo_root);
    if root_project_path.exists() {
        let project = load_project_config(repo_root)?;
        return Ok(Some(LegacyProjectState {
            workspace_root: workspace_root_for_edition(repo_root, &project.edition),
            project,
            is_partial: false,
        }));
    }

    let workspace_candidates = workspace_roots_with_project_config(&editions_root(repo_root))?;
    let has_root_meta_dir = project_meta_dir(repo_root).exists();
    if !has_root_meta_dir && workspace_candidates.is_empty() {
        return Ok(None);
    }

    if workspace_candidates.len() > 1 {
        let candidates = workspace_candidates
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(HackArenaError::msg(format!(
            "Cannot resume incomplete project workspace migration: multiple workspace candidates were found without `{}`: {}.",
            root_project_path.display(),
            candidates
        )));
    }

    let Some(workspace_root) = workspace_candidates.into_iter().next() else {
        return Err(HackArenaError::msg(format!(
            "Cannot resume incomplete project workspace migration: `{}` is missing and no migrated workspace config was found under {}.",
            root_project_path.display(),
            editions_root(repo_root).display()
        )));
    };
    let project = load_project_config(&workspace_root)?;
    Ok(Some(LegacyProjectState {
        workspace_root,
        project,
        is_partial: true,
    }))
}

pub(crate) fn migrate_legacy_root_layout(
    repo_root: &Path,
    state: &LegacyProjectState,
) -> Result<ProjectContext, HackArenaError> {
    if state.is_partial {
        eprintln!(
            "Resuming incomplete project workspace migration into {}...",
            state.workspace_root.display()
        );
    } else {
        eprintln!(
            "Migrating legacy project layout into {}...",
            state.workspace_root.display()
        );
    }

    ensure_dir(&project_meta_dir(&state.workspace_root))?;

    move_legacy_item(
        &repo_root.join(&state.project.backend_dir),
        &state.workspace_root.join(&state.project.backend_dir),
    )?;
    move_legacy_item(
        &repo_root.join(PROJECT_STANDALONE_DIR),
        &state.workspace_root.join(PROJECT_STANDALONE_DIR),
    )?;
    move_legacy_item(
        &repo_root.join(PROJECT_WRAPPERS_DIR),
        &state.workspace_root.join(PROJECT_WRAPPERS_DIR),
    )?;
    move_legacy_item(
        &project_manifest_path(repo_root),
        &project_manifest_path(&state.workspace_root),
    )?;
    move_legacy_item(
        &project_config_path(repo_root),
        &project_config_path(&state.workspace_root),
    )?;

    if !project_config_path(&state.workspace_root).exists() {
        return Err(HackArenaError::msg(format!(
            "Cannot complete project workspace migration into {}: missing {} after migration.",
            state.workspace_root.display(),
            project_config_path(&state.workspace_root).display()
        )));
    }

    save_workspace_control(
        repo_root,
        &WorkspaceControl {
            layout_version: PROJECT_LAYOUT_VERSION_V2,
            active_edition: state.project.edition.clone(),
        },
    )?;

    eprintln!("Project workspace migration complete.");

    Ok(ProjectContext {
        repo_root: repo_root.to_path_buf(),
        workspace_root: state.workspace_root.clone(),
    })
}

fn move_legacy_item(src: &Path, dest: &Path) -> Result<(), HackArenaError> {
    if src.exists() {
        if dest.exists() {
            return Err(HackArenaError::msg(format!(
                "Cannot resume incomplete project workspace migration: both legacy source {} and destination {} exist.",
                src.display(),
                dest.display()
            )));
        }
        if let Some(parent) = dest.parent() {
            ensure_dir(parent)?;
        }
        fs::rename(src, dest).map_err(|e| {
            HackArenaError::msg(format!(
                "Cannot migrate legacy path {} to {}: {}. Release any process using it and retry.",
                src.display(),
                dest.display(),
                e
            ))
        })?;
        return Ok(());
    }

    if dest.exists() {
        return Ok(());
    }

    Ok(())
}
