use crate::archive::{ensure_executable, extract_archive, recreate_dir};
use crate::config::{Paths, ensure_dir};
use crate::download::download_to_dir;
use crate::error::HackArenaError;
use crate::github_releases;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const INTERNAL_SELF_UPDATE_FLAG: &str = "--internal-self-update";
const SESSION_ARG: &str = "--session";
const TOKEN_ARG: &str = "--token";
const SESSION_TTL: Duration = Duration::from_secs(5 * 60);
const FILE_RELEASE_TIMEOUT: Duration = Duration::from_secs(30);
const FILE_RELEASE_POLL_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CliUpdateState {
    UpToDate {
        current_version: String,
    },
    UpdateAvailable {
        current_version: String,
        target_version: String,
        target_tag: String,
    },
    Unknown {
        current_version: String,
    },
    NoRelease {
        current_version: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SelfUpdateSession {
    token: String,
    created_at_unix: u64,
    expires_at_unix: u64,
    source_version: String,
    target_version: String,
    current_binary: PathBuf,
    staged_binary: PathBuf,
    backup_binary: PathBuf,
}

pub(crate) async fn cli_update_state(
    paths: &Paths,
    no_cache: bool,
    prerelease: bool,
) -> CliUpdateState {
    let current_version = env!("CARGO_PKG_VERSION").to_string();
    let Some(current_parsed) = github_releases::parse_release_version(&current_version) else {
        return CliUpdateState::Unknown { current_version };
    };

    let target_tag = match github_releases::latest_self_update_release_tag(
        paths,
        no_cache,
        prerelease,
        Some(&current_version),
    )
    .await
    {
        Ok(Some(tag)) => tag,
        Ok(None) => return CliUpdateState::NoRelease { current_version },
        Err(_) => return CliUpdateState::Unknown { current_version },
    };

    let Some(target_parsed) = github_releases::parse_release_version(&target_tag) else {
        return CliUpdateState::Unknown { current_version };
    };
    let target_version = target_parsed.to_string();

    if target_parsed > current_parsed {
        CliUpdateState::UpdateAvailable {
            current_version,
            target_version,
            target_tag,
        }
    } else {
        CliUpdateState::UpToDate { current_version }
    }
}

pub async fn self_update(
    paths: &Paths,
    no_cache: bool,
    prerelease: bool,
    tag: Option<&str>,
) -> Result<(), HackArenaError> {
    let current_exe = std::env::current_exe().map_err(HackArenaError::Io)?;
    let current_version = github_releases::parse_release_version(env!("CARGO_PKG_VERSION"))
        .ok_or_else(|| {
            HackArenaError::msg(format!(
                "Current CLI version `{}` is not a supported release version.",
                env!("CARGO_PKG_VERSION")
            ))
        })?;

    prepare_self_update_storage(paths)?;

    let target_tag = if let Some(tag) = tag {
        tag.to_string()
    } else {
        let Some(tag_name) = github_releases::latest_self_update_release_tag(
            paths,
            no_cache,
            prerelease,
            Some(env!("CARGO_PKG_VERSION")),
        )
        .await?
        else {
            return Err(HackArenaError::msg(
                "No GitHub release is available for `hackarena-cli`.",
            ));
        };
        tag_name
    };

    let target_version = parse_release_version(&target_tag)?;
    if target_version < current_version {
        if tag.is_some() {
            return Err(HackArenaError::msg(format!(
                "Refusing to downgrade hackarena from `{current_version}` to `{target_version}`."
            )));
        }
        println!("hackarena is up to date ({current_version}).");
        return Ok(());
    }
    if target_version == current_version {
        if let Some(requested_tag) = tag {
            println!("hackarena is already on `{requested_tag}`.");
        } else {
            println!("hackarena is up to date ({current_version}).");
        }
        return Ok(());
    }

    let release = github_releases::self_update_release_by_tag(paths, &target_tag, no_cache, None)
        .await?
        .ok_or_else(|| {
            HackArenaError::msg(format!(
                "Release `{target_tag}` was not found in `INIT-SGGW/HackArena-Cli`."
            ))
        })?;

    println!("Downloading hackarena CLI...");
    let cli_asset_path = download_to_dir(
        paths,
        &paths.self_update_staging_dir(),
        &release.url,
        &release.name,
        &release.sha256,
    )
    .await?;

    let staged_cli_binary = paths
        .self_update_staging_dir()
        .join(platform_binary_name("hackarena"));
    materialize_release_binary(
        &cli_asset_path,
        platform_binary_name("hackarena"),
        &staged_cli_binary,
    )?;

    let updater_binary_path = helper_copy_path(paths, &current_exe);
    fs::copy(&current_exe, &updater_binary_path)
        .map_err(|e| HackArenaError::io_with_path(&updater_binary_path, e))?;
    ensure_executable(&updater_binary_path)?;

    let created_at_unix = now_unix_secs()?;
    let session = SelfUpdateSession {
        token: generate_session_token(&current_exe, &target_tag),
        created_at_unix,
        expires_at_unix: created_at_unix.saturating_add(SESSION_TTL.as_secs()),
        source_version: current_version.to_string(),
        target_version: target_version.to_string(),
        current_binary: current_exe.clone(),
        staged_binary: staged_cli_binary.clone(),
        backup_binary: backup_path_for_executable(paths, &current_exe)?,
    };
    write_session(paths, &session)?;

    println!(
        "Starting self-update from `{}` to `{}`...",
        session.source_version, session.target_version
    );

    let mut cmd = Command::new(&updater_binary_path);
    cmd.arg(INTERNAL_SELF_UPDATE_FLAG)
        .arg(SESSION_ARG)
        .arg(paths.self_update_session_path())
        .arg(TOKEN_ARG)
        .arg(&session.token)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    cmd.spawn()
        .map_err(|e| HackArenaError::io_with_path(&updater_binary_path, e))?;

    println!("Updater launched. This process will now exit.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::CliUpdateState;
    use semver::Version;

    fn classify(current_version: &str, target_tag: Option<&str>) -> CliUpdateState {
        let current_parsed = Version::parse(current_version).expect("current version");
        match target_tag {
            None => CliUpdateState::NoRelease {
                current_version: current_version.to_string(),
            },
            Some(tag) => {
                let target_parsed =
                    Version::parse(tag.trim_start_matches('v')).expect("target tag");
                let target_version = target_parsed.to_string();
                if target_parsed > current_parsed {
                    CliUpdateState::UpdateAvailable {
                        current_version: current_version.to_string(),
                        target_version,
                        target_tag: tag.to_string(),
                    }
                } else {
                    CliUpdateState::UpToDate {
                        current_version: current_version.to_string(),
                    }
                }
            }
        }
    }

    #[test]
    fn cli_update_state_reports_newer_stable_as_update_available() {
        assert!(matches!(
            classify("0.2.0", Some("v0.2.1")),
            CliUpdateState::UpdateAvailable { .. }
        ));
    }

    #[test]
    fn cli_update_state_reports_newer_prerelease_as_update_available() {
        assert!(matches!(
            classify("0.2.0-beta.2", Some("v0.2.0-beta.3")),
            CliUpdateState::UpdateAvailable { .. }
        ));
    }

    #[test]
    fn cli_update_state_keeps_stable_up_to_date_when_only_older_or_equal_target_exists() {
        assert!(matches!(
            classify("0.2.0", Some("v0.2.0")),
            CliUpdateState::UpToDate { .. }
        ));
        assert!(matches!(
            classify("0.2.0", Some("v0.1.9")),
            CliUpdateState::UpToDate { .. }
        ));
    }

    #[test]
    fn cli_update_state_reports_no_release() {
        assert!(matches!(
            classify("0.2.0", None),
            CliUpdateState::NoRelease { .. }
        ));
    }
}

pub fn is_internal_updater_invocation(args: &[OsString]) -> bool {
    args.iter().any(|arg| arg == INTERNAL_SELF_UPDATE_FLAG)
}

pub fn run_internal_updater_from_args() -> Result<(), HackArenaError> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let (session_path, token) = parse_internal_updater_args(&args)?;
    let paths = Paths::discover()?;
    let session = read_and_validate_session(&paths, &session_path, &token)?;

    let result = (|| -> Result<(), HackArenaError> {
        wait_until_releasable(&session.current_binary)?;
        swap_binary_with_backup(
            &session.current_binary,
            &session.staged_binary,
            &session.backup_binary,
        )
    })();

    let cleanup_result = cleanup_after_updater_run(&paths, &session_path, &session.backup_binary);
    result?;
    cleanup_result?;

    println!(
        "Updated hackarena from `{}` to `{}` at {}",
        session.source_version,
        session.target_version,
        session.current_binary.display()
    );
    Ok(())
}

fn parse_internal_updater_args(args: &[OsString]) -> Result<(PathBuf, String), HackArenaError> {
    let invalid = || {
        HackArenaError::msg(
            "This binary is internal and can only be launched by `hackarena self-update`.",
        )
    };

    if !args.iter().any(|arg| arg == INTERNAL_SELF_UPDATE_FLAG) {
        return Err(invalid());
    }

    let mut session_path: Option<PathBuf> = None;
    let mut token: Option<String> = None;

    let mut i = 0usize;
    while i < args.len() {
        let arg = &args[i];
        if arg == INTERNAL_SELF_UPDATE_FLAG {
            i += 1;
            continue;
        }
        if arg == SESSION_ARG {
            let Some(value) = args.get(i + 1) else {
                return Err(invalid());
            };
            session_path = Some(PathBuf::from(value));
            i += 2;
            continue;
        }
        if arg == TOKEN_ARG {
            let Some(value) = args.get(i + 1) else {
                return Err(invalid());
            };
            token = Some(value.to_string_lossy().to_string());
            i += 2;
            continue;
        }
        return Err(invalid());
    }

    let Some(session_path) = session_path else {
        return Err(invalid());
    };
    let Some(token) = token else {
        return Err(invalid());
    };
    if token.trim().is_empty() {
        return Err(invalid());
    }
    Ok((session_path, token))
}

fn prepare_self_update_storage(paths: &Paths) -> Result<(), HackArenaError> {
    ensure_dir(&paths.self_update_root())?;
    ensure_dir(&paths.self_update_backups_dir())?;
    recreate_dir(&paths.self_update_bin_dir())?;
    recreate_dir(&paths.self_update_staging_dir())?;
    if paths.self_update_session_path().exists() {
        fs::remove_file(paths.self_update_session_path())
            .map_err(|e| HackArenaError::io_with_path(paths.self_update_session_path(), e))?;
    }
    Ok(())
}

fn parse_release_version(tag: &str) -> Result<Version, HackArenaError> {
    github_releases::parse_release_version(tag).ok_or_else(|| {
        HackArenaError::msg(format!(
            "Release tag `{tag}` is not a supported version for self-update."
        ))
    })
}

fn materialize_release_binary(
    asset_path: &Path,
    expected_binary_name: &str,
    output_path: &Path,
) -> Result<(), HackArenaError> {
    if let Some(parent) = output_path.parent() {
        ensure_dir(parent)?;
    }
    if output_path.exists() {
        fs::remove_file(output_path).map_err(|e| HackArenaError::io_with_path(output_path, e))?;
    }

    let asset_name = asset_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if asset_name.ends_with(".exe") {
        fs::copy(asset_path, output_path)
            .map_err(|e| HackArenaError::io_with_path(output_path, e))?;
        ensure_executable(output_path)?;
        return Ok(());
    }

    let unpack_dir = output_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{expected_binary_name}.unpack"));
    recreate_dir(&unpack_dir)?;
    extract_archive(asset_path, &unpack_dir)?;
    let extracted =
        find_unique_named_file(&unpack_dir, expected_binary_name)?.ok_or_else(|| {
            HackArenaError::msg(format!(
                "Archive `{}` does not contain expected binary `{expected_binary_name}`.",
                asset_path.display()
            ))
        })?;

    fs::copy(&extracted, output_path).map_err(|e| HackArenaError::io_with_path(output_path, e))?;
    ensure_executable(output_path)?;
    fs::remove_dir_all(&unpack_dir).map_err(|e| HackArenaError::io_with_path(&unpack_dir, e))?;
    Ok(())
}

fn find_unique_named_file(
    root: &Path,
    expected_file_name: &str,
) -> Result<Option<PathBuf>, HackArenaError> {
    let mut matches = Vec::new();
    collect_named_files(root, expected_file_name, &mut matches)?;
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.into_iter().next()),
        _ => Err(HackArenaError::msg(format!(
            "Archive extraction produced multiple `{expected_file_name}` files under `{}`.",
            root.display()
        ))),
    }
}

fn collect_named_files(
    dir: &Path,
    expected_file_name: &str,
    matches: &mut Vec<PathBuf>,
) -> Result<(), HackArenaError> {
    for entry in fs::read_dir(dir).map_err(|e| HackArenaError::io_with_path(dir, e))? {
        let entry = entry.map_err(HackArenaError::Io)?;
        let path = entry.path();
        if path.is_dir() {
            collect_named_files(&path, expected_file_name, matches)?;
            continue;
        }
        if path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case(expected_file_name))
        {
            matches.push(path);
        }
    }
    Ok(())
}

fn backup_path_for_executable(
    paths: &Paths,
    current_exe: &Path,
) -> Result<PathBuf, HackArenaError> {
    let file_name = current_exe.file_name().ok_or_else(|| {
        HackArenaError::msg(format!(
            "Cannot determine executable file name for `{}`.",
            current_exe.display()
        ))
    })?;
    Ok(paths
        .self_update_backups_dir()
        .join(format!("{}.bak", file_name.to_string_lossy())))
}

fn helper_copy_path(paths: &Paths, current_exe: &Path) -> PathBuf {
    let extension = current_exe
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let file_name = if extension.is_empty() {
        "hackarena-self-update".to_string()
    } else {
        format!("hackarena-self-update.{extension}")
    };
    paths.self_update_bin_dir().join(file_name)
}

fn write_session(paths: &Paths, session: &SelfUpdateSession) -> Result<(), HackArenaError> {
    let session_path = paths.self_update_session_path();
    if let Some(parent) = session_path.parent() {
        ensure_dir(parent)?;
    }
    let data = serde_json::to_vec_pretty(session)
        .map_err(|e| HackArenaError::json_with_path(&session_path, e))?;
    fs::write(&session_path, data).map_err(|e| HackArenaError::io_with_path(&session_path, e))
}

fn read_and_validate_session(
    paths: &Paths,
    session_path: &Path,
    token: &str,
) -> Result<SelfUpdateSession, HackArenaError> {
    let invalid = || {
        HackArenaError::msg(
            "This binary is internal and can only be launched by `hackarena self-update`.",
        )
    };

    if session_path != paths.self_update_session_path() {
        return Err(invalid());
    }
    if !session_path.starts_with(paths.self_update_root()) {
        return Err(invalid());
    }

    let data = fs::read(session_path).map_err(|e| HackArenaError::io_with_path(session_path, e))?;
    let session: SelfUpdateSession = serde_json::from_slice(&data)
        .map_err(|e| HackArenaError::json_with_path(session_path, e))?;

    if session.token != token {
        return Err(invalid());
    }
    let now = now_unix_secs()?;
    if now > session.expires_at_unix {
        return Err(HackArenaError::msg(
            "Internal self-update session expired. Run `hackarena self-update` again.",
        ));
    }
    if !session.current_binary.is_absolute() || !session.staged_binary.is_absolute() {
        return Err(invalid());
    }
    if !session
        .backup_binary
        .starts_with(paths.self_update_backups_dir())
    {
        return Err(invalid());
    }
    if !session
        .staged_binary
        .starts_with(paths.self_update_staging_dir())
    {
        return Err(invalid());
    }
    if !session.current_binary.exists() {
        return Err(HackArenaError::msg(format!(
            "Current hackarena binary not found at `{}`.",
            session.current_binary.display()
        )));
    }
    if !session.staged_binary.exists() {
        return Err(HackArenaError::msg(format!(
            "Staged hackarena binary not found at `{}`.",
            session.staged_binary.display()
        )));
    }
    Ok(session)
}

fn wait_until_releasable(target_path: &Path) -> Result<(), HackArenaError> {
    let deadline = SystemTime::now()
        .checked_add(FILE_RELEASE_TIMEOUT)
        .ok_or_else(|| HackArenaError::msg("Invalid updater timeout calculation."))?;

    loop {
        match try_open_for_replace(target_path) {
            Ok(()) => return Ok(()),
            Err(err) => {
                if SystemTime::now() >= deadline {
                    return Err(HackArenaError::msg(format!(
                        "Timed out waiting for `{}` to become writable: {err}",
                        target_path.display()
                    )));
                }
                thread::sleep(FILE_RELEASE_POLL_INTERVAL);
            }
        }
    }
}

fn try_open_for_replace(target_path: &Path) -> Result<(), std::io::Error> {
    #[cfg(windows)]
    {
        let _file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(target_path)?;
    }
    #[cfg(not(windows))]
    {
        let _file = fs::OpenOptions::new().read(true).open(target_path)?;
    }
    Ok(())
}

fn swap_binary_with_backup(
    current_binary: &Path,
    staged_binary: &Path,
    backup_binary: &Path,
) -> Result<(), HackArenaError> {
    let target_dir = current_binary.parent().ok_or_else(|| {
        HackArenaError::msg(format!(
            "Cannot determine parent directory for `{}`.",
            current_binary.display()
        ))
    })?;
    ensure_dir(target_dir)?;
    if let Some(parent) = backup_binary.parent() {
        ensure_dir(parent)?;
    }

    let current_file_name = current_binary
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| {
            HackArenaError::msg(format!(
                "Cannot determine executable file name for `{}`.",
                current_binary.display()
            ))
        })?;
    let replacement_path = target_dir.join(format!("{current_file_name}.new"));
    let old_path = target_dir.join(format!("{current_file_name}.old"));
    let backup_tmp = backup_binary.with_extension("tmp");

    if backup_tmp.exists() {
        fs::remove_file(&backup_tmp).map_err(|e| HackArenaError::io_with_path(&backup_tmp, e))?;
    }
    fs::copy(current_binary, &backup_tmp)
        .map_err(|e| HackArenaError::io_with_path(&backup_tmp, e))?;
    if backup_binary.exists() {
        fs::remove_file(backup_binary)
            .map_err(|e| HackArenaError::io_with_path(backup_binary, e))?;
    }
    fs::rename(&backup_tmp, backup_binary)
        .map_err(|e| HackArenaError::io_with_path(backup_binary, e))?;

    if replacement_path.exists() {
        fs::remove_file(&replacement_path)
            .map_err(|e| HackArenaError::io_with_path(&replacement_path, e))?;
    }
    if old_path.exists() {
        fs::remove_file(&old_path).map_err(|e| HackArenaError::io_with_path(&old_path, e))?;
    }

    fs::copy(staged_binary, &replacement_path)
        .map_err(|e| HackArenaError::io_with_path(&replacement_path, e))?;
    ensure_executable(&replacement_path)?;

    if let Err(err) = fs::rename(current_binary, &old_path) {
        let _ = fs::remove_file(&replacement_path);
        return Err(HackArenaError::io_with_path(current_binary, err));
    }
    if let Err(err) = fs::rename(&replacement_path, current_binary) {
        let _ = fs::rename(&old_path, current_binary);
        let _ = fs::remove_file(&replacement_path);
        return Err(HackArenaError::io_with_path(current_binary, err));
    }
    if old_path.exists() {
        fs::remove_file(&old_path).map_err(|e| HackArenaError::io_with_path(&old_path, e))?;
    }
    if replacement_path.exists() {
        fs::remove_file(&replacement_path)
            .map_err(|e| HackArenaError::io_with_path(&replacement_path, e))?;
    }
    Ok(())
}

fn cleanup_after_updater_run(
    paths: &Paths,
    session_path: &Path,
    backup_binary: &Path,
) -> Result<(), HackArenaError> {
    prune_dir_except(
        &paths.self_update_backups_dir(),
        &[backup_binary.to_path_buf()],
    )?;
    recreate_dir(&paths.self_update_staging_dir())?;
    if session_path.exists() {
        fs::remove_file(session_path).map_err(|e| HackArenaError::io_with_path(session_path, e))?;
    }
    Ok(())
}

fn prune_dir_except(dir: &Path, keep: &[PathBuf]) -> Result<(), HackArenaError> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir).map_err(|e| HackArenaError::io_with_path(dir, e))? {
        let entry = entry.map_err(HackArenaError::Io)?;
        let path = entry.path();
        if keep.iter().any(|keep_path| keep_path == &path) {
            continue;
        }
        if path.is_dir() {
            fs::remove_dir_all(&path).map_err(|e| HackArenaError::io_with_path(&path, e))?;
        } else {
            fs::remove_file(&path).map_err(|e| HackArenaError::io_with_path(&path, e))?;
        }
    }
    Ok(())
}

fn generate_session_token(current_exe: &Path, target_tag: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(current_exe.to_string_lossy().as_bytes());
    hasher.update(target_tag.as_bytes());
    hasher.update(now_unix_secs().unwrap_or_default().to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    hex::encode(hasher.finalize())
}

fn now_unix_secs() -> Result<u64, HackArenaError> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| HackArenaError::msg("System clock is before UNIX_EPOCH."))?
        .as_secs())
}

fn platform_binary_name(stem: &str) -> &'static str {
    match stem {
        "hackarena" => {
            if cfg!(windows) {
                "hackarena.exe"
            } else {
                "hackarena"
            }
        }
        _ => unreachable!("unexpected platform binary stem"),
    }
}
