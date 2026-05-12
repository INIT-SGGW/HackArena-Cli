use crate::config::workspace_migrations::migrate_legacy_root_layout;
use crate::config::{
    ProjectConfig, ensure_dir, load_project_config, project_config_path, save_project_config,
};
use crate::constants::{
    PROJECT_BACKEND_DIR, PROJECT_EDITIONS_DIR, PROJECT_META_DIR, PROJECT_WORKSPACE_FILE,
};
use crate::error::HackArenaError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const PROJECT_LAYOUT_VERSION_V2: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceControl {
    pub layout_version: u32,
    pub active_edition: String,
}

#[derive(Debug, Clone)]
pub struct ProjectContext {
    pub repo_root: PathBuf,
    pub workspace_root: PathBuf,
}

pub fn edition_slug(edition: &str) -> String {
    edition.replace('.', "_")
}

pub fn editions_root(repo_root: &Path) -> PathBuf {
    repo_root.join(PROJECT_EDITIONS_DIR)
}

pub fn workspace_control_path(repo_root: &Path) -> PathBuf {
    repo_root
        .join(PROJECT_META_DIR)
        .join(PROJECT_WORKSPACE_FILE)
}

pub fn workspace_root_for_edition(repo_root: &Path, edition: &str) -> PathBuf {
    editions_root(repo_root).join(edition_slug(edition))
}

pub fn load_workspace_control(repo_root: &Path) -> Result<WorkspaceControl, HackArenaError> {
    let path = workspace_control_path(repo_root);
    let bytes = fs::read(&path).map_err(|e| HackArenaError::io_with_path(&path, e))?;
    serde_json::from_slice(&bytes).map_err(|e| HackArenaError::json_with_path(&path, e))
}

pub fn save_workspace_control(
    repo_root: &Path,
    control: &WorkspaceControl,
) -> Result<(), HackArenaError> {
    let meta_dir = repo_root.join(PROJECT_META_DIR);
    ensure_dir(&meta_dir)?;
    let path = workspace_control_path(repo_root);
    let bytes =
        serde_json::to_vec_pretty(control).map_err(|e| HackArenaError::json_with_path(&path, e))?;
    fs::write(&path, bytes).map_err(|e| HackArenaError::io_with_path(&path, e))?;
    Ok(())
}

pub fn resolve_project_context(start: &Path) -> Result<Option<ProjectContext>, HackArenaError> {
    if let Some(project_root) = find_project_root(start) {
        if let Some(repo_root) = repo_root_from_workspace_root(&project_root) {
            let _project = load_project_config(&project_root)?;
            return Ok(Some(ProjectContext {
                repo_root,
                workspace_root: project_root,
            }));
        }

        return migrate_or_resolve_legacy_root(&project_root).map(Some);
    }

    let Some(repo_root) = find_workspace_repo_root(start) else {
        return Ok(None);
    };
    let control = load_workspace_control(&repo_root)?;
    let workspace_root = workspace_root_for_edition(&repo_root, &control.active_edition);
    if !project_config_path(&workspace_root).exists() {
        return Ok(None);
    }
    Ok(Some(ProjectContext {
        repo_root,
        workspace_root,
    }))
}

pub fn ensure_workspace_for_edition(
    start: &Path,
    edition: &str,
) -> Result<ProjectContext, HackArenaError> {
    let repo_root = if let Some(ctx) = resolve_project_context(start)? {
        ctx.repo_root
    } else {
        start.to_path_buf()
    };

    let workspace_root = workspace_root_for_edition(&repo_root, edition);
    if !project_config_path(&workspace_root).exists() {
        let cfg = ProjectConfig {
            edition: edition.to_string(),
            wrapper_id: None,
            backend_dir: PathBuf::from(PROJECT_BACKEND_DIR),
        };
        save_project_config(&workspace_root, &cfg)?;
    }

    save_workspace_control(
        &repo_root,
        &WorkspaceControl {
            layout_version: PROJECT_LAYOUT_VERSION_V2,
            active_edition: edition.to_string(),
        },
    )?;

    Ok(ProjectContext {
        repo_root,
        workspace_root,
    })
}

fn migrate_or_resolve_legacy_root(repo_root: &Path) -> Result<ProjectContext, HackArenaError> {
    if workspace_control_path(repo_root).exists() {
        let control = load_workspace_control(repo_root)?;
        let workspace_root = workspace_root_for_edition(repo_root, &control.active_edition);
        return Ok(ProjectContext {
            repo_root: repo_root.to_path_buf(),
            workspace_root,
        });
    }

    migrate_legacy_root_layout(repo_root)
}

fn find_project_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|ancestor| project_config_path(ancestor).exists())
        .map(Path::to_path_buf)
}

fn find_workspace_repo_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|ancestor| workspace_control_path(ancestor).exists())
        .map(Path::to_path_buf)
}

fn repo_root_from_workspace_root(workspace_root: &Path) -> Option<PathBuf> {
    let editions_dir = workspace_root.parent()?;
    if editions_dir.file_name()?.to_str()? != PROJECT_EDITIONS_DIR {
        return None;
    }
    editions_dir.parent().map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::{
        PROJECT_LAYOUT_VERSION_V2, edition_slug, ensure_workspace_for_edition,
        load_workspace_control, resolve_project_context, workspace_control_path,
        workspace_root_for_edition,
    };
    use crate::config::{
        ProjectConfig, project_config_path, project_manifest_path, save_project_config,
    };
    use crate::constants::{PROJECT_BACKEND_DIR, PROJECT_STANDALONE_DIR, PROJECT_WRAPPERS_DIR};
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn edition_slug_replaces_dot_with_underscore() {
        assert_eq!(edition_slug("3"), "3");
        assert_eq!(edition_slug("2.5"), "2_5");
    }

    #[test]
    fn ensure_workspace_for_edition_creates_root_control_and_workspace_config() {
        let dir = tempfile::tempdir().expect("temp dir");

        let ctx = ensure_workspace_for_edition(dir.path(), "3").expect("workspace");
        assert_eq!(ctx.repo_root, dir.path());
        assert_eq!(
            ctx.workspace_root,
            workspace_root_for_edition(dir.path(), "3")
        );
        assert!(project_config_path(&ctx.workspace_root).exists());

        let control = load_workspace_control(dir.path()).expect("workspace control");
        assert_eq!(control.layout_version, PROJECT_LAYOUT_VERSION_V2);
        assert_eq!(control.active_edition, "3");
    }

    #[test]
    fn resolve_project_context_migrates_legacy_root_layout() {
        let dir = tempfile::tempdir().expect("temp dir");
        let project = ProjectConfig {
            edition: "3".to_string(),
            wrapper_id: Some("python".to_string()),
            backend_dir: PathBuf::from(PROJECT_BACKEND_DIR),
        };
        save_project_config(dir.path(), &project).expect("save config");
        fs::write(project_manifest_path(dir.path()), "{}").expect("write manifest");
        fs::create_dir_all(dir.path().join(PROJECT_BACKEND_DIR)).expect("backend dir");
        fs::create_dir_all(dir.path().join(PROJECT_STANDALONE_DIR)).expect("standalone dir");
        fs::create_dir_all(dir.path().join(PROJECT_WRAPPERS_DIR)).expect("wrappers dir");

        let ctx = resolve_project_context(dir.path())
            .expect("context result")
            .expect("context");

        assert_eq!(
            ctx.workspace_root,
            workspace_root_for_edition(dir.path(), "3")
        );
        assert!(workspace_control_path(dir.path()).exists());
        assert!(project_config_path(&ctx.workspace_root).exists());
        assert!(project_manifest_path(&ctx.workspace_root).exists());
        assert!(ctx.workspace_root.join(PROJECT_BACKEND_DIR).exists());
        assert!(ctx.workspace_root.join(PROJECT_STANDALONE_DIR).exists());
        assert!(ctx.workspace_root.join(PROJECT_WRAPPERS_DIR).exists());
        assert!(!project_config_path(dir.path()).exists());
    }

    #[test]
    fn resolve_project_context_handles_direct_workspace_path() {
        let dir = tempfile::tempdir().expect("temp dir");
        let ctx = ensure_workspace_for_edition(dir.path(), "3").expect("workspace");
        let nested = ctx.workspace_root.join("wrappers");
        fs::create_dir_all(&nested).expect("nested dir");

        let resolved = resolve_project_context(&nested)
            .expect("context result")
            .expect("context");
        assert_eq!(resolved.repo_root, dir.path());
        assert_eq!(resolved.workspace_root, ctx.workspace_root);
    }

    #[test]
    fn resolve_project_context_fails_when_migration_destination_exists() {
        let dir = tempfile::tempdir().expect("temp dir");
        let project = ProjectConfig {
            edition: "3".to_string(),
            wrapper_id: None,
            backend_dir: PathBuf::from(PROJECT_BACKEND_DIR),
        };
        save_project_config(dir.path(), &project).expect("save config");
        fs::create_dir_all(workspace_root_for_edition(dir.path(), "3")).expect("dest exists");

        let err = resolve_project_context(dir.path()).expect_err("should fail");
        assert!(
            err.to_string()
                .contains("destination workspace already exists")
        );
    }
}
