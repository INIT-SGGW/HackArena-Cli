use crate::cmd_hint;
use crate::config::Paths;
use crate::error::HackArenaError;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn run_auth(paths: &Paths, args: &[String]) -> Result<(), HackArenaError> {
    let auth_bin = resolve_auth_binary(paths)?;
    let status = Command::new(&auth_bin)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| HackArenaError::io_with_path(&auth_bin, e))?;

    if status.success() {
        return Ok(());
    }

    match status.code() {
        Some(code) => Err(HackArenaError::msg(format!(
            "ha-auth exited with code {code}."
        ))),
        None => Err(HackArenaError::msg("ha-auth terminated unexpectedly.")),
    }
}

pub(crate) fn resolve_auth_binary(paths: &Paths) -> Result<PathBuf, HackArenaError> {
    let preferred = if cfg!(windows) {
        paths.bin_dir.join("ha-auth.exe")
    } else {
        paths.bin_dir.join("ha-auth")
    };
    if preferred.exists() {
        return Ok(preferred);
    }

    if let Some(found) = find_any_auth_binary(&paths.bin_dir)? {
        return Ok(found);
    }

    Err(HackArenaError::msg(format!(
        "ha-auth is not installed in {}. Run `{}` first.",
        paths.bin_dir.display(),
        cmd_hint::run_cli("install auth")
    )))
}

fn find_any_auth_binary(bin_dir: &Path) -> Result<Option<PathBuf>, HackArenaError> {
    let rd = match std::fs::read_dir(bin_dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(HackArenaError::io_with_path(bin_dir, e)),
    };

    let mut candidates = Vec::<PathBuf>::new();
    for entry in rd {
        let entry = entry.map_err(HackArenaError::Io)?;
        let path = entry.path();
        let ft = entry
            .file_type()
            .map_err(|e| HackArenaError::io_with_path(&path, e))?;
        if !ft.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if !name.starts_with("ha-auth") {
            continue;
        }
        if cfg!(windows) && !name.to_ascii_lowercase().ends_with(".exe") {
            continue;
        }
        candidates.push(path);
    }

    candidates.sort();
    Ok(candidates.into_iter().next())
}
