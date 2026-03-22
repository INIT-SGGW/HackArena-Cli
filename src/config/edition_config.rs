use serde::{Deserialize, Serialize};

/// A backend source for a given edition.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BackendSource {
    /// Downloads the backend from a direct URL.
    Url { url: String },
}

/// Backend configuration for a given edition. If missing, the edition has no backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendConfig {
    pub source: BackendSource,
    pub sha256: String,
}

/// Artifact descriptor for an edition config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactSpec {
    pub filename: String,
    pub sha256: String,
}

/// Wrapper descriptor for an edition config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WrapperSpec {
    pub id: String,
    pub filename: String,
    pub sha256: String,
}

/// Per-edition configuration stored under the data directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditionConfig {
    pub edition: String,
    #[serde(default)]
    pub backend: Option<BackendConfig>,
    pub auth_artifact: ArtifactSpec,
    pub wrappers: Vec<WrapperSpec>,
    pub default_wrapper_id: String,
    pub bin_name_auth: String,
}

impl EditionConfig {
    /// Finds a wrapper spec by id.
    pub fn wrapper(&self, id: &str) -> Option<&WrapperSpec> {
        self.wrappers.iter().find(|w| w.id == id)
    }
}
