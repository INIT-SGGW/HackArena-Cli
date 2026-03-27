use crate::archive::{ensure_executable, extract_archive, recreate_dir};
use crate::cmd_hint;
use crate::config::{
    EditionConfig, Paths, ProjectConfig, ProjectInstalledBinary, ProjectInstalledBundle,
    ProjectManifest, ensure_dir, is_project_dir, load_project_config, load_project_manifest,
    project_meta_dir, save_project_config, save_project_manifest, validate_edition,
};
use crate::constants::{PROJECT_BACKEND_DIR, PROJECT_WRAPPERS_DIR};
use crate::download::{download_to_cache, sha256_file_hex};
use crate::error::HackArenaError;
use crate::github_releases::{self, LinuxLibcMode};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Sets the active edition and ensures the per-edition config exists.
pub async fn use_edition(_paths: &Paths, edition: &str) -> Result<(), HackArenaError> {
    validate_edition(edition)?;

    let cwd = std::env::current_dir().map_err(HackArenaError::Io)?;
    ensure_dir(&project_meta_dir(&cwd))?;

    let mut cfg = if is_project_dir(&cwd) {
        load_project_config(&cwd)?
    } else {
        ProjectConfig {
            edition: edition.to_string(),
            wrapper_id: None,
            backend_dir: PathBuf::from(PROJECT_BACKEND_DIR),
        }
    };

    cfg.edition = edition.to_string();
    save_project_config(&cwd, &cfg)?;

    println!("Project edition set to `{edition}` (source: GitHub Releases).");
    Ok(())
}

/// Installs missing components using the current directory as a project.
pub async fn install(
    paths: &Paths,
    skip_wrapper: bool,
    no_cache: bool,
    prerelease: bool,
    linux_libc: Option<LinuxLibcMode>,
) -> Result<(), HackArenaError> {
    let cwd = std::env::current_dir().map_err(HackArenaError::Io)?;
    if !is_project_dir(&cwd) {
        println!("No `./.hackarena/project.json` found in {}.", cwd.display());
        println!("Run `{}` first.", cmd_hint::run_cli("use <edition>"));
        return Ok(());
    }

    let project = load_project_config(&cwd)?;
    validate_edition(&project.edition)?;
    let backend_dir = cwd.join(&project.backend_dir);
    let mut manifest = load_project_manifest(&cwd)?;

    let config = load_effective_config(paths, &project, no_cache, prerelease, linux_libc).await?;
    let installed_wrappers = discover_installed_wrappers(&cwd);
    let mut chosen_wrapper = choose_wrapper_id(&project, &config, &installed_wrappers)?;
    if !skip_wrapper && installed_wrappers.is_empty() && chosen_wrapper.is_none() {
        chosen_wrapper = choose_wrapper_for_fresh_install(&project.edition, &config)?;
    }
    if !skip_wrapper && !installed_wrappers.is_empty() {
        println!("Installed wrappers: {}", installed_wrappers.join(", "));
        println!(
            "Use `{}` to install another wrapper.",
            cmd_hint::run_cli("install wrapper")
        );
    }
    let mut backend_installed_now = false;
    let mut wrapper_installed_now = false;

    install_auth_with_config(paths, &config).await?;
    if config.backend.is_some() {
        if backend_dir.exists() {
            if backend_dir_needs_repair(&manifest, &project.backend_dir, &backend_dir)? {
                println!(
                    "Backend exists at {} but looks incomplete/untracked. Reinstalling.",
                    backend_dir.display()
                );
                install_backend_to_dir(paths, &config, &backend_dir, true).await?;
                backend_installed_now = true;
            } else {
                println!("Backend already exists at {}", backend_dir.display());
            }
        } else {
            install_backend_to_dir(paths, &config, &backend_dir, false).await?;
            backend_installed_now = true;
        }
    } else if github_releases::has_backend_repo(&project.edition) {
        println!("Warning: backend release is not available yet on GitHub for this edition.");
    }
    if !skip_wrapper {
        for wrapper_id in github_releases::wrapper_ids_for_edition(&project.edition) {
            if config.wrapper(wrapper_id).is_none() {
                println!("Warning: wrapper `{wrapper_id}` release is not available yet on GitHub.");
            }
        }
        if let Some(wrapper_id) = chosen_wrapper.as_deref() {
            let managed_wrapper_id = github_releases::wrapper_base_id(wrapper_id);
            let wrapper_dir = cwd.join(PROJECT_WRAPPERS_DIR).join(wrapper_id);
            if wrapper_dir.exists() {
                if validate_wrapper_install_layout(wrapper_id, &wrapper_dir).is_ok() {
                    println!(
                        "Wrapper `{wrapper_id}` already exists at {}",
                        wrapper_dir.display()
                    );
                } else if let Some(wrapper) = config.wrapper(managed_wrapper_id) {
                    let preserve_existing_user = wrapper_dir.join("user").is_dir();
                    println!(
                        "Wrapper `{wrapper_id}` at {} has invalid layout. Reinstalling.",
                        wrapper_dir.display()
                    );
                    install_wrapper_to_dir(
                        paths,
                        &project.edition,
                        wrapper_id,
                        managed_wrapper_id,
                        &wrapper_dir,
                        no_cache,
                        prerelease,
                        preserve_existing_user,
                        None,
                        &wrapper.filename,
                        &wrapper.sha256,
                        linux_libc,
                    )
                    .await?;
                    wrapper_installed_now = true;
                } else {
                    println!(
                        "Warning: wrapper `{wrapper_id}` exists at {} but no release is available yet on GitHub to repair it.",
                        wrapper_dir.display()
                    );
                }
            } else {
                if let Some(wrapper) = config.wrapper(managed_wrapper_id) {
                    install_wrapper_to_dir(
                        paths,
                        &project.edition,
                        wrapper_id,
                        managed_wrapper_id,
                        &wrapper_dir,
                        no_cache,
                        prerelease,
                        false,
                        None,
                        &wrapper.filename,
                        &wrapper.sha256,
                        linux_libc,
                    )
                    .await?;
                    wrapper_installed_now = true;
                }
            }
        }
    }

    manifest.auth = Some(ProjectInstalledBinary {
        path: paths.bin_dir.join(&config.bin_name_auth),
        sha256: sha256_file_hex(&paths.bin_dir.join(&config.bin_name_auth)).ok(),
        installed_at_unix: Some(unix_time_seconds()),
    });
    if backend_installed_now {
        manifest.backend = resolve_project_backend_manifest(&config).await?;
    }
    if wrapper_installed_now
        && let Some(wrapper_id) = chosen_wrapper.as_deref()
        && let Some(w) = config.wrapper(github_releases::wrapper_base_id(wrapper_id))
    {
        manifest.wrappers.insert(
            wrapper_id.to_string(),
            ProjectInstalledBundle {
                url: w.filename.clone(),
                install_dir: PathBuf::from(PROJECT_WRAPPERS_DIR).join(wrapper_id),
                sha256: Some(w.sha256.clone()),
                installed_at_unix: Some(unix_time_seconds()),
                files: vec![],
            },
        );
    }
    save_project_manifest(&cwd, &manifest)?;

    Ok(())
}

/// Updates installed components in the current project.
pub async fn update(
    paths: &Paths,
    no_cache: bool,
    prerelease: bool,
    linux_libc: Option<LinuxLibcMode>,
) -> Result<(), HackArenaError> {
    update_auth(paths, no_cache, prerelease, linux_libc).await?;
    update_backend(paths, no_cache, prerelease, linux_libc).await?;
    let cwd = std::env::current_dir().map_err(HackArenaError::Io)?;
    let project = load_project_config(&cwd)?;
    let manifest = load_project_manifest(&cwd).unwrap_or_default();
    let wrapper_ids = manifest.wrappers.keys().cloned().collect::<Vec<_>>();
    for wrapper_id in wrapper_ids {
        if !github_releases::has_wrapper_repo(&project.edition, &wrapper_id) {
            println!("Wrapper `{wrapper_id}`: skip update (not configured for this edition).");
            continue;
        }
        update_wrapper(paths, &wrapper_id, no_cache, prerelease, None, linux_libc).await?;
    }
    Ok(())
}

/// Updates global ha-auth if needed (prints up-to-date message too).
pub async fn update_auth(
    paths: &Paths,
    no_cache: bool,
    prerelease: bool,
    linux_libc: Option<LinuxLibcMode>,
) -> Result<(), HackArenaError> {
    let cwd = std::env::current_dir().map_err(HackArenaError::Io)?;
    if !is_project_dir(&cwd) {
        return Err(HackArenaError::msg(format!(
            "No `./.hackarena/project.json` found. Run `{}` first.",
            cmd_hint::run_cli("install")
        )));
    }

    let project = load_project_config(&cwd)?;
    validate_edition(&project.edition)?;
    let config = load_effective_config(paths, &project, no_cache, prerelease, linux_libc).await?;

    let auth_path = paths.bin_dir.join(&config.bin_name_auth);
    let current_sha = sha256_file_hex(&auth_path).ok();
    if let Some(current) = current_sha.as_deref() {
        if current.eq_ignore_ascii_case(&config.auth_artifact.sha256) {
            println!("ha-auth is already up to date.");
            return Ok(());
        }
    }

    // Re-install is safe: installer skips if file exists, so delete first when updating.
    if auth_path.exists() {
        tokio::fs::remove_file(&auth_path)
            .await
            .map_err(|e| HackArenaError::io_with_path(&auth_path, e))?;
    }
    install_auth_with_config(paths, &config).await?;
    Ok(())
}

/// Updates the backend in the current project by downloading the latest configured backend.
pub async fn update_backend(
    paths: &Paths,
    no_cache: bool,
    prerelease: bool,
    linux_libc: Option<LinuxLibcMode>,
) -> Result<(), HackArenaError> {
    let cwd = std::env::current_dir().map_err(HackArenaError::Io)?;
    if !is_project_dir(&cwd) {
        return Err(HackArenaError::msg(format!(
            "No `./.hackarena/project.json` found. Run `{}` first.",
            cmd_hint::run_cli("install")
        )));
    }

    let project = load_project_config(&cwd)?;
    validate_edition(&project.edition)?;
    let mut config =
        load_effective_config(paths, &project, no_cache, prerelease, linux_libc).await?;
    if config.backend.is_none() {
        return Err(HackArenaError::msg(format!(
            "No backend release is available yet for edition `{}`.",
            project.edition
        )));
    }

    let current_project_manifest = load_project_manifest(&cwd).unwrap_or_default();
    // Compare against the backend pinned by the cached/latest GitHub release metadata.
    if let (Some(current), Some(expected)) = (
        current_project_manifest.backend.as_ref(),
        resolve_project_backend_manifest(&config).await?,
    ) {
        let current_sha = current.sha256.as_deref();
        let expected_sha = expected.sha256.as_deref();
        if current_sha.is_some()
            && expected_sha.is_some()
            && current_sha == expected_sha
            && current.url == expected.url
        {
            println!("Backend is already up to date.");
            return Ok(());
        }
    }

    let backend_dir = cwd.join(&project.backend_dir);
    if let Err(err) = install_backend_to_dir(paths, &config, &backend_dir, true).await {
        if matches!(err, HackArenaError::ChecksumMismatch { .. }) {
            config = load_effective_config(paths, &project, true, prerelease, linux_libc).await?;
            install_backend_to_dir(paths, &config, &backend_dir, true).await?;
        } else {
            return Err(err);
        }
    }

    let mut manifest = load_project_manifest(&cwd)?;
    manifest.backend = resolve_project_backend_manifest(&config).await?;
    save_project_manifest(&cwd, &manifest)?;

    Ok(())
}

/// Installs only ha-auth (global).
pub async fn install_auth(
    paths: &Paths,
    no_cache: bool,
    prerelease: bool,
    linux_libc: Option<LinuxLibcMode>,
) -> Result<(), HackArenaError> {
    let cwd = std::env::current_dir().map_err(HackArenaError::Io)?;
    if !is_project_dir(&cwd) {
        return Err(HackArenaError::msg(format!(
            "No project found. Run `{}` in your project first.",
            cmd_hint::run_cli("use <edition>")
        )));
    }
    let project = load_project_config(&cwd)?;
    let config = load_effective_config(paths, &project, no_cache, prerelease, linux_libc).await?;
    install_auth_with_config(paths, &config).await?;
    print_path_hint(paths);
    Ok(())
}

/// Installs only backend bundle into `./backend` (project-local).
pub async fn install_backend(
    paths: &Paths,
    no_cache: bool,
    prerelease: bool,
    linux_libc: Option<LinuxLibcMode>,
) -> Result<(), HackArenaError> {
    let cwd = std::env::current_dir().map_err(HackArenaError::Io)?;
    let project = if is_project_dir(&cwd) {
        load_project_config(&cwd)?
    } else {
        return Err(HackArenaError::msg(format!(
            "No project found. Run `{}` first.",
            cmd_hint::run_cli("use <edition>")
        )));
    };

    let config = load_effective_config(paths, &project, no_cache, prerelease, linux_libc).await?;
    if config.backend.is_none() {
        return Err(HackArenaError::msg(format!(
            "No backend release is available yet for edition `{}`.",
            project.edition
        )));
    }
    let mut manifest = load_project_manifest(&cwd)?;
    let backend_dir = cwd.join(&project.backend_dir);
    if backend_dir.exists() {
        if backend_dir_needs_repair(&manifest, &project.backend_dir, &backend_dir)? {
            println!(
                "Backend exists at {} but looks incomplete/untracked. Reinstalling.",
                backend_dir.display()
            );
            install_backend_to_dir(paths, &config, &backend_dir, true).await?;
            manifest.backend = resolve_project_backend_manifest(&config).await?;
            save_project_manifest(&cwd, &manifest)?;
            return Ok(());
        }
        println!("Backend already exists at {}", backend_dir.display());
        return Ok(());
    }
    install_backend_to_dir(paths, &config, &backend_dir, false).await?;
    manifest.backend = resolve_project_backend_manifest(&config).await?;
    save_project_manifest(&cwd, &manifest)?;
    Ok(())
}

/// Installs a wrapper by id into `./wrappers/<wrapper_id>` (project-local).
pub async fn install_wrapper(
    paths: &Paths,
    wrapper_id: Option<&str>,
    no_cache: bool,
    prerelease: bool,
    release_tag: Option<&str>,
    linux_libc: Option<LinuxLibcMode>,
) -> Result<(), HackArenaError> {
    let cwd = std::env::current_dir().map_err(HackArenaError::Io)?;
    let project = if is_project_dir(&cwd) {
        load_project_config(&cwd)?
    } else {
        return Err(HackArenaError::msg(format!(
            "No project found. Run `{}` first.",
            cmd_hint::run_cli("use <edition>")
        )));
    };

    let config = load_effective_config(paths, &project, no_cache, prerelease, linux_libc).await?;
    let requested_wrapper_id = match wrapper_id {
        Some(id) => id.to_string(),
        None => choose_wrapper_for_install_command(&project.edition, &config)?,
    };

    let wrappers_root = cwd.join(PROJECT_WRAPPERS_DIR);
    ensure_dir(&wrappers_root)?;
    let managed_wrapper_id = github_releases::wrapper_base_id(&requested_wrapper_id).to_string();
    let requested_is_base = requested_wrapper_id == managed_wrapper_id;
    let mut target_wrapper_id = requested_wrapper_id;
    if requested_is_base {
        let base_dir = wrappers_root.join(&managed_wrapper_id);
        if base_dir.exists()
            && validate_wrapper_install_layout(&managed_wrapper_id, &base_dir).is_ok()
        {
            let next = next_wrapper_instance_id(&wrappers_root, &managed_wrapper_id);
            if !confirm_install_new_wrapper_instance(&managed_wrapper_id, &next)? {
                println!(
                    "Skipped. To install explicitly later, run `{}`.",
                    cmd_hint::run_cli(&format!("install wrapper {next}"))
                );
                return Ok(());
            }
            target_wrapper_id = next;
        }
    }
    let wrapper_dir = wrappers_root.join(&target_wrapper_id);
    let (wrapper_url, wrapper_sha) = resolve_wrapper_target(
        paths,
        &project,
        &config,
        &managed_wrapper_id,
        no_cache,
        prerelease,
        release_tag,
        linux_libc,
    )
    .await?;
    let preserve_existing_user = if wrapper_dir.exists() {
        if validate_wrapper_install_layout(&target_wrapper_id, &wrapper_dir).is_ok() {
            println!(
                "Wrapper `{}` already exists at {}.",
                target_wrapper_id,
                wrapper_dir.display()
            );
            return Ok(());
        }
        println!(
            "Wrapper `{}` at {} has invalid layout. Reinstalling.",
            target_wrapper_id,
            wrapper_dir.display()
        );
        wrapper_dir.join("user").is_dir()
    } else {
        false
    };

    install_wrapper_to_dir(
        paths,
        &project.edition,
        &target_wrapper_id,
        &managed_wrapper_id,
        &wrapper_dir,
        no_cache,
        prerelease,
        preserve_existing_user,
        release_tag,
        &wrapper_url,
        &wrapper_sha,
        linux_libc,
    )
    .await?;

    let mut manifest = load_project_manifest(&cwd)?;
    manifest.wrappers.insert(
        target_wrapper_id.clone(),
        ProjectInstalledBundle {
            url: wrapper_url,
            install_dir: PathBuf::from(PROJECT_WRAPPERS_DIR).join(&target_wrapper_id),
            sha256: Some(wrapper_sha),
            installed_at_unix: Some(unix_time_seconds()),
            files: vec![],
        },
    );
    save_project_manifest(&cwd, &manifest)?;
    Ok(())
}

pub async fn update_wrapper(
    paths: &Paths,
    wrapper_id: &str,
    no_cache: bool,
    prerelease: bool,
    release_tag: Option<&str>,
    linux_libc: Option<LinuxLibcMode>,
) -> Result<(), HackArenaError> {
    let cwd = std::env::current_dir().map_err(HackArenaError::Io)?;
    if !is_project_dir(&cwd) {
        return Err(HackArenaError::msg(format!(
            "No `./.hackarena/project.json` found. Run `{}` first.",
            cmd_hint::run_cli("install")
        )));
    }
    let project = load_project_config(&cwd)?;
    validate_edition(&project.edition)?;

    let wrapper_dir = cwd.join(PROJECT_WRAPPERS_DIR).join(wrapper_id);
    if !wrapper_dir.exists() {
        return Err(HackArenaError::msg(format!(
            "Wrapper `{wrapper_id}` is not installed in {}. Run `{}` first.",
            wrapper_dir.display(),
            cmd_hint::run_cli(&format!("install wrapper {wrapper_id}"))
        )));
    }

    let config = load_effective_config(paths, &project, no_cache, prerelease, linux_libc).await?;
    let managed_wrapper_id = github_releases::wrapper_base_id(wrapper_id).to_string();
    let (wrapper_url, wrapper_sha) = resolve_wrapper_target(
        paths,
        &project,
        &config,
        &managed_wrapper_id,
        no_cache,
        prerelease,
        release_tag,
        linux_libc,
    )
    .await?;

    let mut manifest = load_project_manifest(&cwd)?;
    if let Some(current) = manifest.wrappers.get(wrapper_id) {
        if current.sha256.as_deref() == Some(wrapper_sha.as_str()) && current.url == wrapper_url {
            println!("Wrapper `{wrapper_id}` is already up to date.");
            return Ok(());
        }
    }

    install_wrapper_to_dir(
        paths,
        &project.edition,
        wrapper_id,
        &managed_wrapper_id,
        &wrapper_dir,
        no_cache,
        prerelease,
        true,
        release_tag,
        &wrapper_url,
        &wrapper_sha,
        linux_libc,
    )
    .await?;

    manifest.wrappers.insert(
        wrapper_id.to_string(),
        ProjectInstalledBundle {
            url: wrapper_url,
            install_dir: PathBuf::from(PROJECT_WRAPPERS_DIR).join(wrapper_id),
            sha256: Some(wrapper_sha),
            installed_at_unix: Some(unix_time_seconds()),
            files: vec![],
        },
    );
    save_project_manifest(&cwd, &manifest)?;
    Ok(())
}

async fn resolve_wrapper_target(
    paths: &Paths,
    project: &ProjectConfig,
    config: &EditionConfig,
    managed_wrapper_id: &str,
    no_cache: bool,
    _prerelease: bool,
    release_tag: Option<&str>,
    linux_libc: Option<LinuxLibcMode>,
) -> Result<(String, String), HackArenaError> {
    if let Some(tag) = release_tag {
        if !github_releases::has_wrapper_repo(&project.edition, managed_wrapper_id) {
            return Err(HackArenaError::UnknownWrapper(
                managed_wrapper_id.to_string(),
            ));
        }
        let Some(bundle) = github_releases::wrapper_from_release_tag(
            paths,
            &project.edition,
            managed_wrapper_id,
            tag,
            no_cache,
            linux_libc,
        )
        .await?
        else {
            return Err(HackArenaError::msg(format!(
                "Release tag `{tag}` for wrapper `{managed_wrapper_id}` was not found."
            )));
        };
        let sha = bundle.sha256.clone().ok_or_else(|| {
            HackArenaError::msg(format!(
                "Wrapper `{managed_wrapper_id}` release `{tag}` is missing SHA256 metadata."
            ))
        })?;
        return Ok((bundle.url, sha));
    }

    let Some(wrapper) = config.wrapper(managed_wrapper_id) else {
        if github_releases::has_wrapper_repo(&project.edition, managed_wrapper_id) {
            return Err(HackArenaError::msg(format!(
                "No GitHub release for wrapper `{managed_wrapper_id}` yet."
            )));
        }
        return Err(HackArenaError::UnknownWrapper(
            managed_wrapper_id.to_string(),
        ));
    };
    Ok((wrapper.filename.clone(), wrapper.sha256.clone()))
}

async fn install_auth_with_config(
    paths: &Paths,
    config: &EditionConfig,
) -> Result<(), HackArenaError> {
    ensure_dir(&paths.bin_dir)?;
    ensure_dir(&paths.downloads_cache_dir())?;
    ensure_dir(&paths.data_root())?;

    let dest = paths.bin_dir.join(&config.bin_name_auth);
    if dest.exists() {
        println!(
            "Global `{}` already exists at {}",
            config.bin_name_auth,
            dest.display()
        );
        return Ok(());
    }

    let url = config.auth_artifact.filename.clone();
    let cache_filename = filename_from_url(&url).unwrap_or_else(|| config.bin_name_auth.clone());
    println!("Downloading auth...");
    let cached =
        download_to_cache(paths, &url, &cache_filename, &config.auth_artifact.sha256).await?;

    println!("Installing auth...");
    if is_archive_path(&cached) {
        let tmp = extract_to_temp_dir(paths, &cached)?;
        let extracted = find_extracted_file(tmp.path(), &config.bin_name_auth)?;
        install_file_atomic(&extracted, &dest)?;
    } else {
        install_file_atomic(&cached, &dest)?;
    }
    ensure_executable(&dest)?;

    println!("Installed `{}` to {}", config.bin_name_auth, dest.display());
    Ok(())
}

async fn install_backend_to_dir(
    paths: &Paths,
    config: &EditionConfig,
    install_dir: &Path,
    force_replace: bool,
) -> Result<(), HackArenaError> {
    ensure_dir(&paths.downloads_cache_dir())?;
    if let Some(parent) = install_dir.parent() {
        ensure_dir(parent)?;
    }

    let Some((url, cache_filename, sha256)) = resolve_backend_download(config).await? else {
        return Err(HackArenaError::msg(format!(
            "Edition `{}` has no backend configured.",
            config.edition
        )));
    };

    if install_dir.exists() && !force_replace {
        return Err(HackArenaError::msg(format!(
            "Backend directory already exists at {}",
            install_dir.display()
        )));
    }

    println!("Downloading backend...");
    let cached = download_to_cache(paths, &url, &cache_filename, &sha256).await?;
    println!("Installing backend...");
    recreate_dir(install_dir)?;
    extract_archive(&cached, install_dir)?;
    if force_replace {
        println!("Updated backend at {}", install_dir.display());
    } else {
        println!("Installed backend to {}", install_dir.display());
    }
    Ok(())
}

async fn install_wrapper_to_dir(
    paths: &Paths,
    edition: &str,
    wrapper_instance_id: &str,
    managed_wrapper_id: &str,
    install_dir: &Path,
    no_cache: bool,
    prerelease: bool,
    preserve_existing_user: bool,
    release_tag: Option<&str>,
    wrapper_url: &str,
    wrapper_sha: &str,
    linux_libc: Option<LinuxLibcMode>,
) -> Result<(), HackArenaError> {
    ensure_dir(&paths.downloads_cache_dir())?;
    if let Some(parent) = install_dir.parent() {
        ensure_dir(parent)?;
    }

    let cache_filename =
        filename_from_url(wrapper_url).unwrap_or_else(|| "wrapper.zip".to_string());
    println!("Downloading wrapper `{managed_wrapper_id}`...");
    let cached = download_to_cache(paths, wrapper_url, &cache_filename, wrapper_sha).await?;

    println!("Installing wrapper `{wrapper_instance_id}`...");
    deploy_wrapper_archive(
        wrapper_instance_id,
        &cached,
        install_dir,
        preserve_existing_user,
    )?;
    validate_wrapper_install_layout(wrapper_instance_id, install_dir)?;

    if preserve_existing_user {
        println!(
            "Updated wrapper `{}` at {} (preserved `user/`).",
            wrapper_instance_id,
            install_dir.display()
        );
    } else {
        println!(
            "Installed wrapper `{}` to {}",
            wrapper_instance_id,
            install_dir.display()
        );
    }
    install_wrapper_runtime(
        paths,
        edition,
        managed_wrapper_id,
        install_dir,
        no_cache,
        prerelease,
        release_tag,
        linux_libc,
    )
    .await?;

    Ok(())
}

async fn install_wrapper_runtime(
    paths: &Paths,
    edition: &str,
    managed_wrapper_id: &str,
    install_dir: &Path,
    no_cache: bool,
    prerelease: bool,
    release_tag: Option<&str>,
    linux_libc: Option<LinuxLibcMode>,
) -> Result<(), HackArenaError> {
    if managed_wrapper_id.eq_ignore_ascii_case("python") {
        ensure_python_wrapper_env_api_url(install_dir)?;
        let wheel_artifact = if let Some(tag) = release_tag {
            github_releases::wrapper_python_wheel_from_release_tag(
                paths,
                edition,
                managed_wrapper_id,
                tag,
                no_cache,
                linux_libc,
            )
            .await?
        } else {
            github_releases::latest_wrapper_python_wheel_from_releases(
                paths,
                edition,
                managed_wrapper_id,
                no_cache,
                prerelease,
                linux_libc,
            )
            .await?
        };
        if let Some(wheel_artifact) = wheel_artifact {
            let wheel_url = wheel_artifact.filename;
            let wheel_cache_filename = filename_from_url(&wheel_url)
                .unwrap_or_else(|| "hackarena3-py3-none-any.whl".to_string());
            println!("Downloading Python runtime...");
            let wheel_cached = download_to_cache(
                paths,
                &wheel_url,
                &wheel_cache_filename,
                &wheel_artifact.sha256,
            )
            .await?;
            println!("Installing Python runtime...");
            let vendored_req_path = vendor_python_wheel(install_dir, &wheel_cached)?;
            ensure_python_requirements_has_wheel(install_dir, &vendored_req_path)?;
            print_python_runtime_hint(&vendored_req_path);
        }
        return Ok(());
    }

    if managed_wrapper_id.eq_ignore_ascii_case("csharp") {
        let nupkg_artifact = if let Some(tag) = release_tag {
            github_releases::wrapper_csharp_nupkg_from_release_tag(
                paths,
                edition,
                managed_wrapper_id,
                tag,
                no_cache,
                linux_libc,
            )
            .await?
        } else {
            github_releases::latest_wrapper_csharp_nupkg_from_releases(
                paths,
                edition,
                managed_wrapper_id,
                no_cache,
                prerelease,
                linux_libc,
            )
            .await?
        };
        let Some(nupkg_artifact) = nupkg_artifact else {
            return Err(HackArenaError::msg(format!(
                "No runtime package release is available yet for wrapper `{managed_wrapper_id}`."
            )));
        };

        let nupkg_url = nupkg_artifact.filename;
        let nupkg_cache_filename = filename_from_url(&nupkg_url)
            .unwrap_or_else(|| "HackArena3.Wrapper.CSharp.nupkg".to_string());
        println!("Downloading C# runtime...");
        let nupkg_cached = download_to_cache(
            paths,
            &nupkg_url,
            &nupkg_cache_filename,
            &nupkg_artifact.sha256,
        )
        .await?;
        let runtime_version =
            csharp_runtime_version_from_nupkg_url(&nupkg_url).ok_or_else(|| {
                HackArenaError::msg(format!(
                    "Cannot derive runtime version from C# package asset `{nupkg_cache_filename}`."
                ))
            })?;
        println!("Installing C# runtime...");
        vendor_csharp_nupkg(install_dir, &nupkg_cached)?;
        ensure_csharp_nuget_config(install_dir)?;
        ensure_csharp_bot_csproj_package_reference(install_dir, &runtime_version)?;
        print_csharp_runtime_hint();
        return Ok(());
    }

    if managed_wrapper_id.eq_ignore_ascii_case("cpp") {
        let sdk_artifact = if let Some(tag) = release_tag {
            github_releases::wrapper_cpp_sdk_from_release_tag(
                paths,
                edition,
                managed_wrapper_id,
                tag,
                no_cache,
                linux_libc,
            )
            .await?
        } else {
            github_releases::latest_wrapper_cpp_sdk_from_releases(
                paths,
                edition,
                managed_wrapper_id,
                no_cache,
                prerelease,
                linux_libc,
            )
            .await?
        };
        let Some(sdk_artifact) = sdk_artifact else {
            return Err(HackArenaError::msg(format!(
                "No runtime package release is available yet for wrapper `{managed_wrapper_id}`."
            )));
        };

        let sdk_url = sdk_artifact.filename;
        let sdk_cache_filename =
            filename_from_url(&sdk_url).unwrap_or_else(|| "hackarena3-cpp-sdk.tar.gz".to_string());
        println!("Downloading C++ SDK runtime (large package, may take a while)...");
        let sdk_cached =
            download_to_cache(paths, &sdk_url, &sdk_cache_filename, &sdk_artifact.sha256).await?;
        println!("Installing C++ SDK runtime...");
        vendor_cpp_sdk_archive(install_dir, &sdk_cached)?;
        ensure_cpp_cmakelists_runtime_include(install_dir)?;
        print_cpp_runtime_hint();
    }

    Ok(())
}

fn deploy_wrapper_archive(
    wrapper_id: &str,
    archive_path: &Path,
    install_dir: &Path,
    preserve_existing_user: bool,
) -> Result<(), HackArenaError> {
    if !preserve_existing_user || !install_dir.exists() {
        recreate_dir(install_dir)?;
        extract_archive(archive_path, install_dir)?;
        return Ok(());
    }

    // Validate new archive before touching existing install dir.
    let validate_tmp = tempfile::tempdir().map_err(HackArenaError::Io)?;
    extract_archive(archive_path, validate_tmp.path())?;
    validate_wrapper_install_layout(wrapper_id, validate_tmp.path())?;

    let existing_user = install_dir.join("user");
    let user_backup_tmp = tempfile::tempdir().map_err(HackArenaError::Io)?;
    let backup_user_dir = user_backup_tmp.path().join("user");
    let has_user = existing_user.is_dir();
    if has_user {
        copy_dir_recursive(&existing_user, &backup_user_dir)?;
    }

    recreate_dir(install_dir)?;
    extract_archive(archive_path, install_dir)?;

    if has_user {
        let target_user = install_dir.join("user");
        if target_user.exists() {
            std::fs::remove_dir_all(&target_user)
                .map_err(|e| HackArenaError::io_with_path(&target_user, e))?;
        }
        copy_dir_recursive(&backup_user_dir, &target_user)?;
    }

    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), HackArenaError> {
    ensure_dir(dst)?;
    let rd = std::fs::read_dir(src).map_err(|e| HackArenaError::io_with_path(src, e))?;
    for entry in rd {
        let entry = entry.map_err(HackArenaError::Io)?;
        let path = entry.path();
        let ft = entry
            .file_type()
            .map_err(|e| HackArenaError::io_with_path(&path, e))?;
        let target = dst.join(entry.file_name());
        if ft.is_dir() {
            copy_dir_recursive(&path, &target)?;
            continue;
        }
        if ft.is_file() {
            std::fs::copy(&path, &target).map_err(|e| HackArenaError::io_with_path(&target, e))?;
        }
    }
    Ok(())
}

fn validate_wrapper_install_layout(
    wrapper_id: &str,
    install_dir: &Path,
) -> Result<(), HackArenaError> {
    let user_dir = install_dir.join("user");
    if !user_dir.is_dir() {
        return Err(HackArenaError::msg(format!(
            "Wrapper `{wrapper_id}` has invalid layout: missing `user/` directory in {}.",
            install_dir.display()
        )));
    }

    let has_manifest = ["manifest.toml", "system/manifest.toml"]
        .iter()
        .any(|rel| install_dir.join(rel).is_file());
    if !has_manifest {
        return Err(HackArenaError::msg(format!(
            "Wrapper `{wrapper_id}` has invalid layout: missing `manifest.toml` (root) or `system/manifest.toml` in {}.",
            install_dir.display()
        )));
    }

    Ok(())
}

fn print_python_runtime_hint(_vendored_req_path: &str) {
    println!("Updated `user/requirements.txt` with Python runtime (`hackarena3`).");
    println!("Required for local run/tests: install dependencies in your virtual environment.");
    println!("Run from this wrapper directory:");
    println!("  python -m pip install -r user/requirements.txt");
}

fn ensure_python_wrapper_env_api_url(install_dir: &Path) -> Result<(), HackArenaError> {
    const ENV_VAR: &str = "HA3_WRAPPER_API_URL";
    const DEFAULT_VALUE: &str = "ha3-api.hackarena.pl";

    let env_path = install_dir.join("user").join(".env");
    if let Some(parent) = env_path.parent() {
        ensure_dir(parent)?;
    }

    let existing = if env_path.is_file() {
        std::fs::read_to_string(&env_path)
            .map_err(|e| HackArenaError::io_with_path(&env_path, e))?
    } else {
        String::new()
    };
    let mut lines: Vec<String> = existing.lines().map(|l| l.to_string()).collect();
    if lines
        .iter()
        .any(|line| line.trim_start().starts_with(&format!("{ENV_VAR}=")))
    {
        return Ok(());
    }
    lines.push(format!("{ENV_VAR}={DEFAULT_VALUE}"));

    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    std::fs::write(&env_path, out).map_err(|e| HackArenaError::io_with_path(&env_path, e))?;
    Ok(())
}

fn vendor_python_wheel(install_dir: &Path, cached_wheel: &Path) -> Result<String, HackArenaError> {
    let wheel_name = cached_wheel
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| HackArenaError::msg("invalid wheel filename"))?;
    let vendor_dir = install_dir.join("user").join(".vendor");
    ensure_dir(&vendor_dir)?;
    let vendor_path = vendor_dir.join(wheel_name);
    std::fs::copy(cached_wheel, &vendor_path)
        .map_err(|e| HackArenaError::io_with_path(&vendor_path, e))?;
    Ok(format!("./user/.vendor/{wheel_name}"))
}

fn vendor_csharp_nupkg(install_dir: &Path, cached_nupkg: &Path) -> Result<String, HackArenaError> {
    let nupkg_name = cached_nupkg
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| HackArenaError::msg("invalid nupkg filename"))?;
    let vendor_dir = install_dir.join("user").join(".vendor").join("nuget");
    ensure_dir(&vendor_dir)?;
    let vendor_path = vendor_dir.join(nupkg_name);
    std::fs::copy(cached_nupkg, &vendor_path)
        .map_err(|e| HackArenaError::io_with_path(&vendor_path, e))?;
    Ok(format!("./.vendor/nuget/{nupkg_name}"))
}

fn vendor_cpp_sdk_archive(
    install_dir: &Path,
    cached_archive: &Path,
) -> Result<PathBuf, HackArenaError> {
    let vendor_dir = install_dir.join("user").join(".vendor").join("cpp");
    recreate_dir(&vendor_dir)?;
    extract_archive(cached_archive, &vendor_dir)?;

    let runtime_cmake = vendor_dir.join("hackarena-runtime.cmake");
    if !runtime_cmake.is_file() {
        let config_rel = find_hackarena3_config_relative_path(&vendor_dir)?;
        let generated = generated_cpp_runtime_cmake(&config_rel);
        std::fs::write(&runtime_cmake, generated)
            .map_err(|e| HackArenaError::io_with_path(&runtime_cmake, e))?;
    }
    Ok(runtime_cmake)
}

fn find_hackarena3_config_relative_path(vendor_dir: &Path) -> Result<String, HackArenaError> {
    let mut matches = Vec::<String>::new();
    let mut stack = vec![vendor_dir.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let rd = std::fs::read_dir(&dir).map_err(|e| HackArenaError::io_with_path(&dir, e))?;
        for entry in rd {
            let entry = entry.map_err(HackArenaError::Io)?;
            let path = entry.path();
            let ft = entry
                .file_type()
                .map_err(|e| HackArenaError::io_with_path(&path, e))?;
            if ft.is_dir() {
                stack.push(path);
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            let is_config = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case("hackarena3Config.cmake"));
            if !is_config {
                continue;
            }
            let rel = path
                .strip_prefix(vendor_dir)
                .map_err(|_| {
                    HackArenaError::msg(format!(
                        "Failed to compute relative path for `{}`.",
                        path.display()
                    ))
                })?
                .to_string_lossy()
                .replace('\\', "/");
            matches.push(rel);
        }
    }

    matches.sort();
    matches.dedup();

    if matches.is_empty() {
        return Err(HackArenaError::msg(format!(
            "C++ SDK package is invalid: missing `hackarena-runtime.cmake` and `hackarena3Config.cmake` in {}.",
            vendor_dir.display()
        )));
    }
    if matches.len() > 1 {
        return Err(HackArenaError::msg(format!(
            "C++ SDK package is ambiguous: multiple `hackarena3Config.cmake` files found in {}: {}",
            vendor_dir.display(),
            matches.join(", ")
        )));
    }
    Ok(matches[0].clone())
}

fn generated_cpp_runtime_cmake(config_rel_path: &str) -> String {
    format!(
        concat!(
            "# Auto-generated by hackarena-cli.\n",
            "include_guard(GLOBAL)\n\n",
            "set(_HA3_RUNTIME_ROOT \"${{CMAKE_CURRENT_LIST_DIR}}\")\n",
            "set(_HA3_CONFIG_PATH \"${{_HA3_RUNTIME_ROOT}}/{config_rel_path}\")\n\n",
            "if(NOT EXISTS \"${{_HA3_CONFIG_PATH}}\")\n",
            "    message(FATAL_ERROR \"HackArena C++ SDK config not found at `${{_HA3_CONFIG_PATH}}`.\")\n",
            "endif()\n\n",
            "include(\"${{_HA3_CONFIG_PATH}}\")\n\n",
            "function(hackarena_use_runtime target_name)\n",
            "    if(NOT TARGET \"${{target_name}}\")\n",
            "        message(FATAL_ERROR \"Target `${{target_name}}` does not exist.\")\n",
            "    endif()\n",
            "    if(NOT TARGET hackarena3::hackarena3)\n",
            "        message(FATAL_ERROR \"Target `hackarena3::hackarena3` is missing after loading `${{_HA3_CONFIG_PATH}}`.\")\n",
            "    endif()\n",
            "    target_link_libraries(\"${{target_name}}\" PRIVATE hackarena3::hackarena3)\n",
            "    if(COMMAND hackarena3_copy_runtime_dlls)\n",
            "        hackarena3_copy_runtime_dlls(\"${{target_name}}\")\n",
            "    endif()\n",
            "endfunction()\n"
        ),
        config_rel_path = config_rel_path
    )
}

fn csharp_runtime_version_from_nupkg_url(url: &str) -> Option<String> {
    const PREFIX: &str = "HackArena3.Wrapper.CSharp.";
    const SUFFIX: &str = ".nupkg";

    let filename = filename_from_url(url)?;
    let lower = filename.to_ascii_lowercase();
    if !lower.starts_with(&PREFIX.to_ascii_lowercase())
        || !lower.ends_with(&SUFFIX.to_ascii_lowercase())
    {
        return None;
    }
    let start = PREFIX.len();
    let end = filename.len().checked_sub(SUFFIX.len())?;
    if end <= start {
        return None;
    }
    Some(filename[start..end].to_string())
}

fn ensure_csharp_nuget_config(install_dir: &Path) -> Result<(), HackArenaError> {
    const CONFIG_NAME: &str = "NuGet.config";
    const SOURCE_KEY: &str = "hackarena-local";
    const SOURCE_VALUE: &str = ".vendor/nuget";

    let config_path = install_dir.join("user").join(CONFIG_NAME);
    if let Some(parent) = config_path.parent() {
        ensure_dir(parent)?;
    }

    let existing = if config_path.is_file() {
        std::fs::read_to_string(&config_path)
            .map_err(|e| HackArenaError::io_with_path(&config_path, e))?
    } else {
        String::new()
    };
    if existing.contains("key=\"hackarena-local\"") || existing.contains("key='hackarena-local'") {
        return Ok(());
    }

    let source_line = format!("    <add key=\"{SOURCE_KEY}\" value=\"{SOURCE_VALUE}\" />");
    let updated = if existing.trim().is_empty() {
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <packageSources>
    <add key="nuget.org" value="https://api.nuget.org/v3/index.json" protocolVersion="3" />
{source_line}
  </packageSources>
</configuration>
"#
        )
    } else if let Some(idx) = existing.find("</packageSources>") {
        let mut out = existing.clone();
        let insertion = format!("{source_line}\n");
        out.insert_str(idx, &insertion);
        out
    } else if let Some(idx) = existing.find("</configuration>") {
        let mut out = existing.clone();
        let insertion = format!("  <packageSources>\n{source_line}\n  </packageSources>\n");
        out.insert_str(idx, &insertion);
        out
    } else {
        let mut out = existing.clone();
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&format!(
            "<packageSources>\n{source_line}\n</packageSources>\n"
        ));
        out
    };

    std::fs::write(&config_path, updated)
        .map_err(|e| HackArenaError::io_with_path(&config_path, e))?;
    Ok(())
}

fn ensure_csharp_bot_csproj_package_reference(
    install_dir: &Path,
    version: &str,
) -> Result<(), HackArenaError> {
    const PACKAGE_ID: &str = "HackArena3.Wrapper.CSharp";
    const START_MARKER: &str = "<!-- hackarena-csharp-runtime: managed by hackarena-cli:start -->";
    const END_MARKER: &str = "<!-- hackarena-csharp-runtime: managed by hackarena-cli:end -->";

    let csproj_path = install_dir.join("user").join("Bot.csproj");
    if !csproj_path.is_file() {
        return Err(HackArenaError::msg(format!(
            "Wrapper `csharp` requires `user/Bot.csproj` in {}.",
            install_dir.display()
        )));
    }
    let existing = std::fs::read_to_string(&csproj_path)
        .map_err(|e| HackArenaError::io_with_path(&csproj_path, e))?;

    let mut filtered = Vec::<String>::new();
    let mut in_managed_block = false;
    let mut skip_package_ref_block = false;
    let package_id_lower = PACKAGE_ID.to_ascii_lowercase();
    for line in existing.lines() {
        let trimmed = line.trim();
        if skip_package_ref_block {
            if trimmed.to_ascii_lowercase().contains("</packagereference>") {
                skip_package_ref_block = false;
            }
            continue;
        }
        if trimmed == START_MARKER {
            in_managed_block = true;
            continue;
        }
        if trimmed == END_MARKER {
            in_managed_block = false;
            continue;
        }
        if in_managed_block {
            continue;
        }
        if is_csharp_runtime_package_reference_line(trimmed, &package_id_lower) {
            if !trimmed.contains("/>") {
                skip_package_ref_block = true;
            }
            continue;
        }
        filtered.push(line.to_string());
    }

    let managed_block = vec![
        format!("  {START_MARKER}"),
        "  <ItemGroup>".to_string(),
        format!("    <PackageReference Include=\"{PACKAGE_ID}\" Version=\"{version}\" />"),
        "  </ItemGroup>".to_string(),
        format!("  {END_MARKER}"),
    ];

    let mut out_lines = Vec::<String>::new();
    let mut inserted = false;
    for line in filtered {
        if !inserted && line.trim() == "</Project>" {
            out_lines.extend(managed_block.clone());
            inserted = true;
        }
        out_lines.push(line);
    }
    if !inserted {
        if !out_lines.is_empty() && !out_lines.last().is_some_and(|l| l.trim().is_empty()) {
            out_lines.push(String::new());
        }
        out_lines.extend(managed_block);
    }

    let mut out = out_lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    std::fs::write(&csproj_path, out).map_err(|e| HackArenaError::io_with_path(&csproj_path, e))?;
    Ok(())
}

fn is_csharp_runtime_package_reference_line(line: &str, package_id_lower: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("<packagereference") && lower.contains(package_id_lower)
}

fn print_csharp_runtime_hint() {
    println!("Configured C# runtime package (`HackArena3.Wrapper.CSharp`) for local development.");
    println!("Run from this wrapper directory:");
    println!("  dotnet run --project user/Bot.csproj");
}

fn ensure_cpp_cmakelists_runtime_include(install_dir: &Path) -> Result<(), HackArenaError> {
    const START_MARKER: &str = "# hackarena-cpp-runtime: managed by hackarena-cli:start";
    const END_MARKER: &str = "# hackarena-cpp-runtime: managed by hackarena-cli:end";
    const MSVC_ONLY_CHECK_START: &str = "if(WIN32 AND NOT MSVC)";
    const MSVC_ONLY_CHECK_BODY: &str = "    message(FATAL_ERROR \"HackArena C++ wrapper on Windows supports only MSVC (Visual Studio/cl.exe). MinGW/GCC is not supported.\")";
    const MSVC_ONLY_CHECK_END: &str = "endif()";
    const INCLUDE_LINE: &str =
        "include(\"${CMAKE_CURRENT_LIST_DIR}/.vendor/cpp/hackarena-runtime.cmake\")";
    const LEGACY_ARTIFACTS_LINE: &str = "set(HACKARENA3_ARTIFACTS_DIR";
    const FIND_PACKAGE_LINE: &str = "find_package(hackarena3 CONFIG REQUIRED)";

    let cmake_path = install_dir.join("user").join("CMakeLists.txt");
    if !cmake_path.is_file() {
        return Err(HackArenaError::msg(format!(
            "Wrapper `cpp` requires `user/CMakeLists.txt` in {}.",
            install_dir.display()
        )));
    }
    let existing = std::fs::read_to_string(&cmake_path)
        .map_err(|e| HackArenaError::io_with_path(&cmake_path, e))?;

    let mut filtered = Vec::<String>::new();
    let mut in_managed_block = false;
    let mut insert_before_idx = None;
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed == START_MARKER {
            in_managed_block = true;
            continue;
        }
        if trimmed == END_MARKER {
            in_managed_block = false;
            continue;
        }
        if in_managed_block {
            continue;
        }
        if trimmed == INCLUDE_LINE {
            continue;
        }
        if insert_before_idx.is_none() && trimmed.starts_with(LEGACY_ARTIFACTS_LINE) {
            insert_before_idx = Some(filtered.len());
        }
        filtered.push(line.to_string());
    }

    let mut normalized = Vec::<String>::new();
    let mut idx = 0usize;
    while idx < filtered.len() {
        let trimmed = filtered[idx].trim();
        if trimmed == FIND_PACKAGE_LINE {
            normalized.push("if(NOT TARGET hackarena3::hackarena3)".to_string());
            normalized.push("    find_package(hackarena3 CONFIG REQUIRED)".to_string());
            normalized.push("endif()".to_string());
            idx += 1;
            continue;
        }
        normalized.push(filtered[idx].clone());
        idx += 1;
    }

    let insert_at = insert_before_idx.unwrap_or(0).min(normalized.len());
    let managed_block = vec![
        START_MARKER.to_string(),
        MSVC_ONLY_CHECK_START.to_string(),
        MSVC_ONLY_CHECK_BODY.to_string(),
        MSVC_ONLY_CHECK_END.to_string(),
        INCLUDE_LINE.to_string(),
        END_MARKER.to_string(),
        String::new(),
    ];

    let mut out_lines = Vec::<String>::new();
    out_lines.extend(normalized[..insert_at].iter().cloned());
    out_lines.extend(managed_block);
    out_lines.extend(normalized[insert_at..].iter().cloned());

    while out_lines.last().is_some_and(|line| line.trim().is_empty()) {
        out_lines.pop();
    }
    let mut out = out_lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    std::fs::write(&cmake_path, out).map_err(|e| HackArenaError::io_with_path(&cmake_path, e))?;
    Ok(())
}

fn print_cpp_runtime_hint() {
    println!("Configured C++ SDK runtime for local development.");
    println!("Run from this wrapper directory:");
    if cfg!(target_os = "windows") {
        println!("  cmake -S user -B user/build -G \"Visual Studio 17 2022\" -A x64");
        println!("  cmake --build user/build --config Release");
        println!(
            "Windows: only MSVC is supported (Visual Studio/cl.exe). MinGW/GCC is not supported."
        );
    } else {
        println!("  cmake -S user -B user/build -DCMAKE_BUILD_TYPE=Release");
        println!("  cmake --build user/build");
    }
}

fn is_hackarena3_requirement_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("hackarena3==")
        || lower.starts_with("hackarena3 @")
        || (lower.contains("hackarena3-") && lower.ends_with(".whl"))
}

fn is_hackarena3_requirement_comment(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with("# hackarena3-runtime:")
}

fn is_user_requirements_hint_comment(line: &str) -> bool {
    line.trim()
        .eq_ignore_ascii_case("# add your own dependencies below")
}

fn ensure_python_requirements_has_wheel(
    install_dir: &Path,
    wheel_url: &str,
) -> Result<(), HackArenaError> {
    const RUNTIME_MARKER: &str = "# hackarena3-runtime: managed by hackarena-cli";
    const USER_HINT: &str = "# add your own dependencies below";

    let req_path = install_dir.join("user").join("requirements.txt");
    if let Some(parent) = req_path.parent() {
        ensure_dir(parent)?;
    }

    let existing = if req_path.is_file() {
        std::fs::read_to_string(&req_path)
            .map_err(|e| HackArenaError::io_with_path(&req_path, e))?
    } else {
        String::new()
    };

    let mut lines: Vec<String> = existing.lines().map(|l| l.to_string()).collect();
    lines.retain(|line| {
        !is_hackarena3_requirement_line(line)
            && !is_hackarena3_requirement_comment(line)
            && !is_user_requirements_hint_comment(line)
    });
    if !lines.iter().any(|line| line.trim() == wheel_url) {
        lines.push(RUNTIME_MARKER.to_string());
        lines.push(wheel_url.to_string());
    }
    if !lines
        .iter()
        .any(|line| line.trim().eq_ignore_ascii_case(USER_HINT))
    {
        lines.push(USER_HINT.to_string());
    }

    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    std::fs::write(&req_path, out).map_err(|e| HackArenaError::io_with_path(&req_path, e))?;
    Ok(())
}

fn install_file_atomic(src: &Path, dest: &Path) -> Result<(), HackArenaError> {
    if let Some(parent) = dest.parent() {
        ensure_dir(parent)?;
    }

    let tmp = dest.with_extension("new");
    std::fs::copy(src, &tmp).map_err(|e| HackArenaError::io_with_path(&tmp, e))?;

    if dest.exists() {
        std::fs::remove_file(dest).map_err(|e| HackArenaError::io_with_path(dest, e))?;
    }
    std::fs::rename(&tmp, dest).map_err(|e| HackArenaError::io_with_path(dest, e))?;
    Ok(())
}

fn is_archive_path(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    name.ends_with(".zip") || name.ends_with(".tar.gz")
}

fn extract_to_temp_dir(paths: &Paths, archive_path: &Path) -> Result<TempDir, HackArenaError> {
    let parent = paths.downloads_cache_dir();
    let dir =
        tempfile::tempdir_in(&parent).map_err(|e| HackArenaError::io_with_path(&parent, e))?;
    extract_archive(archive_path, dir.path())?;
    Ok(dir)
}

fn find_extracted_file(root: &Path, filename: &str) -> Result<PathBuf, HackArenaError> {
    let direct = root.join(filename);
    if direct.exists() {
        return Ok(direct);
    }

    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let rd = std::fs::read_dir(&dir).map_err(|e| HackArenaError::io_with_path(&dir, e))?;
        for entry in rd {
            let entry = entry.map_err(|e| HackArenaError::io_with_path(&dir, e))?;
            let path = entry.path();
            let ft = entry
                .file_type()
                .map_err(|e| HackArenaError::io_with_path(&path, e))?;
            if ft.is_dir() {
                stack.push(path);
                continue;
            }
            if ft.is_file()
                && path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|n| n.eq_ignore_ascii_case(filename))
            {
                return Ok(path);
            }
        }
    }

    Err(HackArenaError::msg(format!(
        "auth archive did not contain expected binary `{filename}`"
    )))
}

fn print_path_hint(paths: &Paths) {
    println!();
    println!("Global bin directory: {}", paths.bin_dir.display());
    if cfg!(windows) {
        println!(
            "If `hackarena` can't find `ha-auth`, add this directory to your PATH (User env var) and open a new terminal."
        );
    } else {
        println!(
            "If `ha-auth` isn't found, add this directory to your PATH (e.g. in `~/.profile`):"
        );
        println!("  export PATH=\"{}:$PATH\"", paths.bin_dir.display());
    }
}

async fn load_effective_config(
    paths: &Paths,
    project: &ProjectConfig,
    no_cache: bool,
    prerelease: bool,
    linux_libc: Option<LinuxLibcMode>,
) -> Result<EditionConfig, HackArenaError> {
    github_releases::load_edition_config_from_cache(
        paths,
        &project.edition,
        no_cache,
        prerelease,
        linux_libc,
    )
    .await
}

async fn resolve_backend_download(
    config: &EditionConfig,
) -> Result<Option<(String, String, String)>, HackArenaError> {
    let backend = match config.backend.as_ref() {
        Some(b) => b,
        None => return Ok(None),
    };

    match &backend.source {
        crate::config::BackendSource::Url { url } => {
            let filename = filename_from_url(url).unwrap_or_else(|| "backend.tar.gz".into());
            Ok(Some((url.clone(), filename, backend.sha256.clone())))
        }
    }
}

async fn resolve_project_backend_manifest(
    config: &EditionConfig,
) -> Result<Option<ProjectInstalledBundle>, HackArenaError> {
    let Some((url, _cache_filename, sha256)) = resolve_backend_download(config).await? else {
        return Ok(None);
    };
    Ok(Some(ProjectInstalledBundle {
        url,
        install_dir: PathBuf::from(PROJECT_BACKEND_DIR),
        sha256: Some(sha256),
        installed_at_unix: Some(unix_time_seconds()),
        files: vec![],
    }))
}

fn filename_from_url(url: &str) -> Option<String> {
    if let Some((_, query)) = url.split_once('?') {
        for item in query.split('&') {
            if let Some(name) = item.strip_prefix("asset_name=")
                && !name.is_empty()
            {
                return Some(name.to_string());
            }
        }
    }

    url.split('?')
        .next()
        .unwrap_or(url)
        .split('/')
        .next_back()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

fn discover_installed_wrappers(cwd: &Path) -> Vec<String> {
    let dir = cwd.join(PROJECT_WRAPPERS_DIR);
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            if let Ok(ft) = e.file_type() {
                if ft.is_dir() {
                    if let Some(name) = e.file_name().to_str() {
                        out.push(name.to_string());
                    }
                }
            }
        }
    }
    out.sort();
    out
}

fn next_wrapper_instance_id(root: &Path, base_id: &str) -> String {
    if !root.join(base_id).exists() {
        return base_id.to_string();
    }
    for idx in 1..usize::MAX {
        let candidate = format!("{base_id}_{idx}");
        if !root.join(&candidate).exists() {
            return candidate;
        }
    }
    base_id.to_string()
}

fn confirm_install_new_wrapper_instance(
    base_id: &str,
    next_id: &str,
) -> Result<bool, HackArenaError> {
    if !(io::stdin().is_terminal() && io::stdout().is_terminal()) {
        println!(
            "Wrapper `{base_id}` already exists. Non-interactive mode detected; not creating `{next_id}` automatically."
        );
        return Ok(false);
    }

    let mut stdout = io::stdout();
    write!(
        &mut stdout,
        "Wrapper `{base_id}` already exists. Install another instance `{next_id}`? [y/N]: "
    )
    .map_err(HackArenaError::Io)?;
    stdout.flush().map_err(HackArenaError::Io)?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(HackArenaError::Io)?;
    let trimmed = input.trim().to_ascii_lowercase();
    Ok(trimmed == "y" || trimmed == "yes")
}

fn backend_dir_needs_repair(
    manifest: &ProjectManifest,
    backend_rel_path: &Path,
    backend_abs_path: &Path,
) -> Result<bool, HackArenaError> {
    let tracked = manifest
        .backend
        .as_ref()
        .is_some_and(|b| b.install_dir == backend_rel_path);
    let has_files = dir_has_entries(backend_abs_path)?;
    Ok(!tracked || !has_files)
}

fn dir_has_entries(path: &Path) -> Result<bool, HackArenaError> {
    if !path.is_dir() {
        return Ok(false);
    }
    let mut rd = std::fs::read_dir(path).map_err(|e| HackArenaError::io_with_path(path, e))?;
    Ok(rd.next().is_some())
}

fn choose_wrapper_id(
    project: &ProjectConfig,
    config: &EditionConfig,
    installed: &[String],
) -> Result<Option<String>, HackArenaError> {
    if let Some(wrapper_id) = project.wrapper_id.as_ref() {
        if config
            .wrapper(github_releases::wrapper_base_id(wrapper_id))
            .is_some()
        {
            return Ok(Some(wrapper_id.clone()));
        }
    }
    if installed.len() == 1 {
        return Ok(Some(installed[0].clone()));
    }
    if installed.is_empty() {
        if config.wrappers.len() == 1 {
            return Ok(Some(config.wrappers[0].id.clone()));
        }
        return Ok(None);
    }
    Ok(None)
}

fn wrapper_choices_for_edition(edition: &str, config: &EditionConfig) -> Vec<(String, bool)> {
    let mut ids = github_releases::wrapper_ids_for_edition(edition)
        .into_iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>();
    if ids.is_empty() {
        ids = config.wrappers.iter().map(|w| w.id.clone()).collect();
    }
    ids.sort();
    ids.dedup();
    ids.into_iter()
        .map(|id| {
            let available = config
                .wrapper(github_releases::wrapper_base_id(&id))
                .is_some();
            (id, available)
        })
        .collect()
}

fn print_available_wrappers(choices: &[(String, bool)]) {
    println!("Available wrappers:");
    for (id, available) in choices {
        if *available {
            println!("  - {id}");
        } else {
            println!("  - {id} (no release yet)");
        }
    }
}

fn choose_wrapper_for_fresh_install(
    edition: &str,
    config: &EditionConfig,
) -> Result<Option<String>, HackArenaError> {
    let choices = wrapper_choices_for_edition(edition, config);
    let installable = choices
        .iter()
        .filter(|(_, available)| *available)
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    if installable.is_empty() {
        return Ok(None);
    }
    if installable.len() == 1 {
        return Ok(Some(installable[0].clone()));
    }
    if !(io::stdin().is_terminal() && io::stdout().is_terminal()) {
        println!(
            "Multiple wrappers are available ({}) but interactive selection is unavailable.",
            installable.join(", ")
        );
        println!(
            "Use `{}` to pick one explicitly.",
            cmd_hint::run_cli("install wrapper <id>")
        );
        return Ok(None);
    }

    println!("No wrappers installed yet. Choose wrapper to install:");
    choose_wrapper_from_list(&installable, "Choose wrapper number: ").map(Some)
}

fn choose_wrapper_for_install_command(
    edition: &str,
    config: &EditionConfig,
) -> Result<String, HackArenaError> {
    let choices = wrapper_choices_for_edition(edition, config);
    if choices.is_empty() {
        return Err(HackArenaError::msg(format!(
            "No wrappers are configured for edition `{edition}`."
        )));
    }
    print_available_wrappers(&choices);

    let installable = choices
        .iter()
        .filter(|(_, available)| *available)
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    if installable.is_empty() {
        return Err(HackArenaError::msg(
            "No wrapper release is available yet on GitHub for this edition.",
        ));
    }
    if installable.len() == 1 {
        println!("Using wrapper `{}`.", installable[0]);
        return Ok(installable[0].clone());
    }
    if !(io::stdin().is_terminal() && io::stdout().is_terminal()) {
        return Err(HackArenaError::msg(format!(
            "Wrapper id is required in non-interactive mode. Installable wrappers: {}.",
            installable.join(", ")
        )));
    }

    println!("Choose wrapper to install:");
    choose_wrapper_from_list(&installable, "Choose wrapper number: ")
}

fn choose_wrapper_from_list(options: &[String], prompt: &str) -> Result<String, HackArenaError> {
    for (idx, id) in options.iter().enumerate() {
        println!("  {}. {}", idx + 1, id);
    }
    print!("{prompt}");
    io::stdout().flush().map_err(HackArenaError::Io)?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(HackArenaError::Io)?;
    let index = input
        .trim()
        .parse::<usize>()
        .map_err(|_| HackArenaError::msg("Invalid wrapper number."))?;
    if index == 0 || index > options.len() {
        return Err(HackArenaError::msg(
            "Selected wrapper number is out of range.",
        ));
    }
    Ok(options[index - 1].clone())
}

fn unix_time_seconds() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::{
        csharp_runtime_version_from_nupkg_url, deploy_wrapper_archive,
        ensure_cpp_cmakelists_runtime_include, ensure_csharp_bot_csproj_package_reference,
        ensure_csharp_nuget_config, ensure_python_requirements_has_wheel,
        ensure_python_wrapper_env_api_url, validate_wrapper_install_layout, vendor_cpp_sdk_archive,
        vendor_csharp_nupkg, vendor_python_wheel,
    };
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::fs;
    use std::io::Write;
    use std::path::Path;
    use tar::Builder;
    use zip::write::SimpleFileOptions;

    fn create_wrapper_zip(path: &Path, entries: &[(&str, &str)]) {
        let file = fs::File::create(path).expect("create zip");
        let mut zip = zip::ZipWriter::new(file);
        let opts =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, content) in entries {
            zip.start_file(name, opts).expect("start file");
            zip.write_all(content.as_bytes()).expect("write file");
        }
        zip.finish().expect("finish zip");
    }

    fn create_tar_gz(path: &Path, entries: &[(&str, &str)]) {
        let file = fs::File::create(path).expect("create tar.gz");
        let encoder = GzEncoder::new(file, Compression::default());
        let mut tar = Builder::new(encoder);
        for (name, content) in entries {
            let bytes = content.as_bytes();
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, *name, bytes)
                .expect("append file");
        }
        let encoder = tar.into_inner().expect("finish tar");
        encoder.finish().expect("finish gzip");
    }

    #[test]
    fn wrapper_layout_validation_accepts_root_manifest() {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::create_dir_all(dir.path().join("user").join("src")).expect("user dir");
        fs::write(dir.path().join("manifest.toml"), "schema = 1").expect("manifest");

        validate_wrapper_install_layout("python", dir.path()).expect("layout should pass");
    }

    #[test]
    fn wrapper_layout_validation_accepts_system_manifest() {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::create_dir_all(dir.path().join("user")).expect("user dir");
        fs::create_dir_all(dir.path().join("system")).expect("system dir");
        fs::write(
            dir.path().join("system").join("manifest.toml"),
            "schema = 1",
        )
        .expect("manifest");

        validate_wrapper_install_layout("python", dir.path()).expect("layout should pass");
    }

    #[test]
    fn wrapper_layout_validation_fails_when_user_missing() {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::write(dir.path().join("manifest.toml"), "schema = 1").expect("manifest");

        let err = validate_wrapper_install_layout("python", dir.path()).expect_err("should fail");
        assert!(err.to_string().contains("missing `user/`"));
    }

    #[test]
    fn wrapper_layout_validation_fails_when_manifest_missing() {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::create_dir_all(dir.path().join("user")).expect("user dir");

        let err = validate_wrapper_install_layout("python", dir.path()).expect_err("should fail");
        assert!(err.to_string().contains("missing `manifest.toml`"));
    }

    #[test]
    fn ensure_python_requirements_has_wheel_adds_and_updates_single_entry() {
        let dir = tempfile::tempdir().expect("temp dir");
        let req_path = dir.path().join("user").join("requirements.txt");
        fs::create_dir_all(req_path.parent().expect("parent")).expect("mkdir");
        fs::write(
            &req_path,
            "requests==2.32.0\nhttps://github.com/INIT-SGGW/HackArena3.0-ApiWrapper-Python/releases/download/v0.1.0/hackarena3-0.1.0-py3-none-any.whl\n# hackarena3-runtime: managed by hackarena-cli\n./.vendor/hackarena3-0.0.9-py3-none-any.whl\n",
        )
        .expect("write");

        let wheel = "./user/.vendor/hackarena3-0.1.0b1-py3-none-any.whl";
        ensure_python_requirements_has_wheel(dir.path(), wheel).expect("update req");
        ensure_python_requirements_has_wheel(dir.path(), wheel).expect("idempotent");

        let content = fs::read_to_string(&req_path).expect("read");
        let lines = content.lines().collect::<Vec<_>>();
        assert_eq!(
            lines,
            vec![
                "requests==2.32.0",
                "# hackarena3-runtime: managed by hackarena-cli",
                wheel,
                "# add your own dependencies below"
            ]
        );
    }

    #[test]
    fn vendor_python_wheel_places_file_in_user_vendor_dir() {
        let dir = tempfile::tempdir().expect("temp dir");
        let install_dir = dir.path().join("wrapper");
        fs::create_dir_all(install_dir.join("user")).expect("user dir");

        let cached = dir.path().join("hackarena3-0.1.0b1-py3-none-any.whl");
        fs::write(&cached, b"wheel-bytes").expect("write cached");

        let rel = vendor_python_wheel(&install_dir, &cached).expect("vendor");
        assert_eq!(rel, "./user/.vendor/hackarena3-0.1.0b1-py3-none-any.whl");
        let vendored = install_dir
            .join("user")
            .join(".vendor")
            .join("hackarena3-0.1.0b1-py3-none-any.whl");
        assert!(vendored.is_file());
    }

    #[test]
    fn ensure_python_wrapper_env_api_url_creates_env_when_missing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let install_dir = dir.path().join("wrapper");
        fs::create_dir_all(install_dir.join("user")).expect("user dir");

        ensure_python_wrapper_env_api_url(&install_dir).expect("create env");
        let content = fs::read_to_string(install_dir.join("user").join(".env")).expect("read env");
        assert_eq!(content, "HA3_WRAPPER_API_URL=ha3-api.hackarena.pl\n");
    }

    #[test]
    fn ensure_python_wrapper_env_api_url_appends_when_key_missing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let install_dir = dir.path().join("wrapper");
        fs::create_dir_all(install_dir.join("user")).expect("user dir");
        fs::write(install_dir.join("user").join(".env"), "OTHER=1\n").expect("write env");

        ensure_python_wrapper_env_api_url(&install_dir).expect("append key");
        let content = fs::read_to_string(install_dir.join("user").join(".env")).expect("read env");
        assert_eq!(
            content,
            "OTHER=1\nHA3_WRAPPER_API_URL=ha3-api.hackarena.pl\n"
        );
    }

    #[test]
    fn ensure_python_wrapper_env_api_url_keeps_existing_value() {
        let dir = tempfile::tempdir().expect("temp dir");
        let install_dir = dir.path().join("wrapper");
        fs::create_dir_all(install_dir.join("user")).expect("user dir");
        fs::write(
            install_dir.join("user").join(".env"),
            "HA3_WRAPPER_API_URL=http://localhost:8090\n",
        )
        .expect("write env");

        ensure_python_wrapper_env_api_url(&install_dir).expect("no override");
        let content = fs::read_to_string(install_dir.join("user").join(".env")).expect("read env");
        assert_eq!(content, "HA3_WRAPPER_API_URL=http://localhost:8090\n");
    }

    #[test]
    fn csharp_runtime_version_is_extracted_from_asset_url() {
        let url = "https://api.github.com/repos/org/repo/releases/assets/1?asset_name=HackArena3.Wrapper.CSharp.0.1.0-beta.1.nupkg";
        let version = csharp_runtime_version_from_nupkg_url(url).expect("version");
        assert_eq!(version, "0.1.0-beta.1");
    }

    #[test]
    fn vendor_csharp_nupkg_places_file_in_user_vendor_nuget_dir() {
        let dir = tempfile::tempdir().expect("temp dir");
        let install_dir = dir.path().join("wrapper");
        fs::create_dir_all(install_dir.join("user")).expect("user dir");

        let cached = dir
            .path()
            .join("HackArena3.Wrapper.CSharp.0.1.0-beta.1.nupkg");
        fs::write(&cached, b"nupkg-bytes").expect("write cached");

        let rel = vendor_csharp_nupkg(&install_dir, &cached).expect("vendor");
        assert_eq!(
            rel,
            "./.vendor/nuget/HackArena3.Wrapper.CSharp.0.1.0-beta.1.nupkg"
        );
        let vendored = install_dir
            .join("user")
            .join(".vendor")
            .join("nuget")
            .join("HackArena3.Wrapper.CSharp.0.1.0-beta.1.nupkg");
        assert!(vendored.is_file());
    }

    #[test]
    fn vendor_cpp_sdk_archive_extracts_into_user_vendor_cpp() {
        let dir = tempfile::tempdir().expect("temp dir");
        let install_dir = dir.path().join("wrapper");
        fs::create_dir_all(install_dir.join("user")).expect("user dir");

        let cached = dir
            .path()
            .join("hackarena3-cpp-sdk-0.1.0b8-x86_64-pc-windows-msvc.tar.gz");
        create_tar_gz(
            &cached,
            &[
                ("hackarena-runtime.cmake", "set(HA_RUNTIME 1)\n"),
                (
                    "lib/cmake/hackarena3/hackarena3Config.cmake",
                    "set(HA_CFG 1)\n",
                ),
            ],
        );

        let runtime_path = vendor_cpp_sdk_archive(&install_dir, &cached).expect("vendor");
        assert_eq!(
            runtime_path,
            install_dir
                .join("user")
                .join(".vendor")
                .join("cpp")
                .join("hackarena-runtime.cmake")
        );
        assert!(runtime_path.is_file());
    }

    #[test]
    fn vendor_cpp_sdk_archive_generates_runtime_when_missing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let install_dir = dir.path().join("wrapper");
        fs::create_dir_all(install_dir.join("user")).expect("user dir");

        let cached = dir
            .path()
            .join("hackarena3-cpp-sdk-0.1.0b8-x86_64-pc-windows-msvc.tar.gz");
        create_tar_gz(
            &cached,
            &[(
                "hackarena3-cpp-sdk-0.1.0b8-x86_64-pc-windows-msvc/lib/cmake/hackarena3/hackarena3Config.cmake",
                "set(HA_CFG 1)\n",
            )],
        );

        let runtime_path = vendor_cpp_sdk_archive(&install_dir, &cached).expect("should generate");
        assert!(runtime_path.is_file());
        let content = fs::read_to_string(runtime_path).expect("runtime content");
        assert!(content.contains("Auto-generated by hackarena-cli."));
        assert!(content.contains("hackarena3-cpp-sdk-0.1.0b8-x86_64-pc-windows-msvc/lib/cmake/hackarena3/hackarena3Config.cmake"));
    }

    #[test]
    fn vendor_cpp_sdk_archive_fails_when_runtime_and_config_missing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let install_dir = dir.path().join("wrapper");
        fs::create_dir_all(install_dir.join("user")).expect("user dir");

        let cached = dir
            .path()
            .join("hackarena3-cpp-sdk-0.1.0b8-x86_64-pc-windows-msvc.tar.gz");
        create_tar_gz(&cached, &[("README.md", "no runtime and no config\n")]);

        let err = vendor_cpp_sdk_archive(&install_dir, &cached).expect_err("should reject sdk");
        assert!(err.to_string().contains("hackarena3Config.cmake"));
    }

    #[test]
    fn ensure_csharp_nuget_config_adds_local_source_idempotently() {
        let dir = tempfile::tempdir().expect("temp dir");
        let install_dir = dir.path().join("wrapper");
        fs::create_dir_all(install_dir.join("user")).expect("user dir");
        let cfg_path = install_dir.join("user").join("NuGet.config");
        fs::write(
            &cfg_path,
            r#"<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <packageSources>
    <add key="nuget.org" value="https://api.nuget.org/v3/index.json" protocolVersion="3" />
  </packageSources>
</configuration>
"#,
        )
        .expect("write config");

        ensure_csharp_nuget_config(&install_dir).expect("update config");
        ensure_csharp_nuget_config(&install_dir).expect("idempotent");

        let content = fs::read_to_string(cfg_path).expect("read config");
        assert!(content.contains("key=\"hackarena-local\""));
        assert_eq!(content.matches("key=\"hackarena-local\"").count(), 1);
    }

    #[test]
    fn ensure_csharp_bot_csproj_package_reference_adds_and_updates() {
        let dir = tempfile::tempdir().expect("temp dir");
        let install_dir = dir.path().join("wrapper");
        fs::create_dir_all(install_dir.join("user")).expect("user dir");
        let csproj_path = install_dir.join("user").join("Bot.csproj");
        fs::write(
            &csproj_path,
            r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net8.0</TargetFramework>
  </PropertyGroup>
</Project>
"#,
        )
        .expect("write csproj");

        ensure_csharp_bot_csproj_package_reference(&install_dir, "0.1.0-beta.1")
            .expect("add package");
        ensure_csharp_bot_csproj_package_reference(&install_dir, "0.1.0-beta.2")
            .expect("update package");

        let content = fs::read_to_string(csproj_path).expect("read csproj");
        assert!(content.contains("HackArena3.Wrapper.CSharp"));
        assert!(content.contains("Version=\"0.1.0-beta.2\""));
        assert_eq!(content.matches("HackArena3.Wrapper.CSharp").count(), 1);
    }

    #[test]
    fn ensure_cpp_cmakelists_runtime_include_is_idempotent_and_guards_find_package() {
        let dir = tempfile::tempdir().expect("temp dir");
        let install_dir = dir.path().join("wrapper");
        fs::create_dir_all(install_dir.join("user")).expect("user dir");
        let cmake_path = install_dir.join("user").join("CMakeLists.txt");
        fs::write(
            &cmake_path,
            r#"cmake_minimum_required(VERSION 3.24)
project(hackarena3_cpp_template LANGUAGES CXX)

set(HACKARENA3_ARTIFACTS_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../../artifacts")
find_package(hackarena3 CONFIG REQUIRED)

add_executable(bot src/main.cpp)
target_link_libraries(bot PRIVATE hackarena3::hackarena3)
"#,
        )
        .expect("write cmake");

        ensure_cpp_cmakelists_runtime_include(&install_dir).expect("first patch");
        ensure_cpp_cmakelists_runtime_include(&install_dir).expect("idempotent patch");

        let content = fs::read_to_string(cmake_path).expect("read cmake");
        assert_eq!(
            content
                .matches("# hackarena-cpp-runtime: managed by hackarena-cli:start")
                .count(),
            1
        );
        assert_eq!(
            content
                .matches(
                    "include(\"${CMAKE_CURRENT_LIST_DIR}/.vendor/cpp/hackarena-runtime.cmake\")"
                )
                .count(),
            1
        );
        assert_eq!(content.matches("if(WIN32 AND NOT MSVC)").count(), 1);
        assert!(
            content.contains(
                "HackArena C++ wrapper on Windows supports only MSVC (Visual Studio/cl.exe). MinGW/GCC is not supported."
            )
        );
        assert!(content.contains("if(NOT TARGET hackarena3::hackarena3)"));
        assert!(content.contains("    find_package(hackarena3 CONFIG REQUIRED)"));
        let include_pos = content
            .find("include(\"${CMAKE_CURRENT_LIST_DIR}/.vendor/cpp/hackarena-runtime.cmake\")")
            .expect("include pos");
        let legacy_pos = content
            .find("set(HACKARENA3_ARTIFACTS_DIR")
            .expect("legacy pos");
        assert!(include_pos < legacy_pos);
    }

    #[test]
    fn ensure_cpp_cmakelists_runtime_include_requires_user_cmakelists() {
        let dir = tempfile::tempdir().expect("temp dir");
        let install_dir = dir.path().join("wrapper");
        fs::create_dir_all(install_dir.join("user")).expect("user dir");

        let err = ensure_cpp_cmakelists_runtime_include(&install_dir)
            .expect_err("missing CMakeLists should fail");
        assert!(err.to_string().contains("user/CMakeLists.txt"));
    }

    #[test]
    fn deploy_wrapper_archive_preserves_existing_user_dir_on_update() {
        let dir = tempfile::tempdir().expect("temp dir");
        let install_dir = dir.path().join("wrapper");
        fs::create_dir_all(install_dir.join("user").join("src")).expect("mkdir");
        fs::write(
            install_dir.join("user").join("src").join("main.py"),
            "print('keep')",
        )
        .expect("write user");
        fs::write(install_dir.join("manifest.toml"), "schema = 1").expect("write old manifest");

        let archive = dir.path().join("wrapper-python.zip");
        create_wrapper_zip(
            &archive,
            &[
                ("system/manifest.toml", "schema = 1\n"),
                ("user/src/template.py", "print('template')\n"),
            ],
        );

        deploy_wrapper_archive("python", &archive, &install_dir, true).expect("deploy");

        assert!(install_dir.join("system").join("manifest.toml").is_file());
        assert!(
            install_dir
                .join("user")
                .join("src")
                .join("main.py")
                .is_file()
        );
        assert!(
            !install_dir
                .join("user")
                .join("src")
                .join("template.py")
                .exists()
        );
    }
}
