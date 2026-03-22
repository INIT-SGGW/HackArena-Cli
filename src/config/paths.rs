use crate::error::HackArenaError;
use std::env;
use std::path::PathBuf;

/// Filesystem layout used by the CLI.
#[derive(Debug, Clone)]
pub struct Paths {
    pub data_dir: PathBuf,
    pub config_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub bin_dir: PathBuf,
}

impl Paths {
    /// Discovers OS-appropriate directories per HackArena rules.
    pub fn discover() -> Result<Self, HackArenaError> {
        cfg_if::cfg_if! {
            if #[cfg(windows)] {
                let local_app_data = env::var_os("LOCALAPPDATA")
                    .map(PathBuf::from)
                    .ok_or_else(|| HackArenaError::msg("%LOCALAPPDATA% is not set"))?;
                let app_data = env::var_os("APPDATA")
                    .map(PathBuf::from)
                    .ok_or_else(|| HackArenaError::msg("%APPDATA% is not set"))?;
                let base = local_app_data.join("HackArena");
                Ok(Self {
                    data_dir: base.clone(),
                    config_dir: app_data.join("HackArena"),
                    cache_dir: base.join("cache"),
                    bin_dir: base.join("bin"),
                })
            } else {
                let home = home_dir()?;
                let data_dir = env::var_os("XDG_DATA_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".local").join("share"));
                let config_dir = env::var_os("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".config"));
                let cache_dir = env::var_os("XDG_CACHE_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".cache"));
                let bin_dir = data_dir.join("hackarena").join("bin");
                Ok(Self {
                    data_dir,
                    config_dir,
                    cache_dir,
                    bin_dir,
                })
            }
        }
    }

    /// Returns `<data_dir>/hackarena`.
    pub fn data_root(&self) -> PathBuf {
        self.data_dir.join("hackarena")
    }

    /// Returns `<config_dir>/hackarena`.
    pub fn config_root(&self) -> PathBuf {
        self.config_dir.join("hackarena")
    }

    /// Returns `<cache_dir>/hackarena/downloads`.
    pub fn downloads_cache_dir(&self) -> PathBuf {
        self.cache_dir.join("hackarena").join("downloads")
    }

    /// Returns `<cache_dir>/hackarena/releases`.
    pub fn releases_cache_dir(&self) -> PathBuf {
        self.cache_dir.join("hackarena").join("releases")
    }

    /// Returns `<data_dir>/hackarena/logs`.
    pub fn logs_dir(&self) -> PathBuf {
        self.data_root().join("logs")
    }
}

#[cfg(not(windows))]
fn home_dir() -> Result<PathBuf, HackArenaError> {
    if let Some(home) = env::var_os("HOME") {
        return Ok(PathBuf::from(home));
    }
    Err(HackArenaError::msg(
        "could not determine home directory (HOME/USERPROFILE not set)",
    ))
}
