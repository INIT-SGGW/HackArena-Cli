mod edition_config;
pub mod editions;
mod fs;
mod paths;
mod project;
mod project_manifest;

pub use edition_config::{ArtifactSpec, BackendConfig, BackendSource, EditionConfig, WrapperSpec};
pub use editions::validate_edition;
pub use fs::ensure_dir;
pub use paths::Paths;
pub use project::{
    ProjectConfig, is_project_dir, load_project_config, project_config_path, project_manifest_path,
    project_meta_dir, save_project_config,
};
pub use project_manifest::{
    ProjectInstalledBinary, ProjectInstalledBundle, ProjectManifest, load_project_manifest,
    save_project_manifest,
};
