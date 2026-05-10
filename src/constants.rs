/// Constants used by the `hackarena` bootstrap CLI.
///
/// These are kept in one place to make the installation model easy to adjust.

/// Project metadata directory name.
pub const PROJECT_META_DIR: &str = ".hackarena";

/// Project config file name inside `.hackarena/`.
pub const PROJECT_CONFIG_FILE: &str = "project.json";

/// Project manifest file name inside `.hackarena/`.
pub const PROJECT_MANIFEST_FILE: &str = "manifest.json";

/// Project-local backend directory name (relative to project root).
pub const PROJECT_BACKEND_DIR: &str = "backend";

/// Project-local standalone directory name (relative to project root).
pub const PROJECT_STANDALONE_DIR: &str = "standalone";

/// Project-local wrappers directory name (relative to project root).
pub const PROJECT_WRAPPERS_DIR: &str = "wrappers";
