use crate::cmd_hint;
use crate::config::{
    Paths, ProjectContext, load_project_config, load_project_manifest, project_config_path,
    project_manifest_path, resolve_project_context, validate_edition,
};
use crate::constants::{PROJECT_STANDALONE_DIR, PROJECT_WRAPPERS_DIR};
use crate::download::sha256_file_hex;
use crate::error::HackArenaError;
use crate::github_auth;
use crate::github_releases;
use crate::install::{
    discover_installed_wrappers, standalone_install_layout_issue, wrapper_install_layout_issue,
};
use owo_colors::OwoColorize;
use std::collections::BTreeSet;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
struct WrapperDiskEntry {
    id: String,
    path: PathBuf,
    tracked: bool,
    experimental: bool,
    known_for_edition: bool,
    layout_issue: Option<String>,
}

impl WrapperDiskEntry {
    fn is_untracked(&self) -> bool {
        !self.tracked
    }
}

/// Prints diagnostics for the current installation.
pub async fn doctor(
    paths: &Paths,
    no_cache: bool,
    prerelease: bool,
    verbose: bool,
) -> Result<(), HackArenaError> {
    let cwd = std::env::current_dir().map_err(HackArenaError::Io)?;
    print_header("hackarena doctor");
    let Some((ctx, project)) = load_project_or_warn(&cwd, "install")? else {
        return Ok(());
    };
    let workspace_root = &ctx.workspace_root;
    let auth_path = paths.bin_dir.join(if cfg!(windows) {
        "ha-auth.exe"
    } else {
        "ha-auth"
    });
    let backend_dir = workspace_root.join(&project.backend_dir);
    let standalone_dir = workspace_root.join(PROJECT_STANDALONE_DIR);
    let manifest = load_project_manifest(workspace_root).unwrap_or_default();
    let wrapper_disk_entries =
        discover_wrapper_disk_entries(workspace_root, &project.edition, &manifest);

    println!("Project dir: {}", workspace_root.display());
    println!("Edition: {}", project.edition);
    println!();

    println!("Project");
    print_check(
        "project.json",
        project_config_path(workspace_root).exists(),
        &project_config_path(workspace_root),
    );
    print_check(
        "manifest.json",
        project_manifest_path(workspace_root).exists(),
        &project_manifest_path(workspace_root),
    );
    print_check_value(
        "edition",
        validate_edition(&project.edition).is_ok(),
        &project.edition,
    );
    println!();

    println!("Environment");
    println!("Artifact source: GitHub Releases");
    print_github_auth_status(paths)?;
    if verbose {
        print_verbose_runtime(paths, no_cache, prerelease);
    }
    println!();

    println!("Components");
    print_check_with_action(
        "global ha-auth",
        auth_path.exists(),
        &auth_path,
        Some(cmd_hint::run_cli("install auth")),
    );
    print_check_with_action(
        "backend",
        backend_dir.exists(),
        &backend_dir,
        Some(cmd_hint::run_cli("install backend")),
    );
    print_standalone_doctor_status(&manifest, &standalone_dir);
    print_check_with_action(
        "wrappers/",
        workspace_root.join("wrappers").exists(),
        &workspace_root.join("wrappers"),
        Some(cmd_hint::run_cli("install wrapper")),
    );
    println!();

    println!("Wrappers");
    let mut printed_wrapper_section = false;
    for wrapper_id in manifest.wrappers.keys() {
        if github_releases::is_experimental_wrapper(&project.edition, wrapper_id) {
            printed_wrapper_section = true;
            print_experimental_wrapper_note(wrapper_id, &project.edition);
            print_action(&format!(
                "submit is unsupported for this wrapper in edition {}; local install/update only",
                project.edition
            ));
        }
    }
    for entry in wrapper_disk_entries
        .iter()
        .filter(|entry| entry.is_untracked())
    {
        printed_wrapper_section = true;
        print_untracked_wrapper_warning(entry);
    }
    if !printed_wrapper_section {
        println!("wrapper: no additional diagnostics");
    }

    Ok(())
}

fn print_header(title: &str) {
    println!("{title}");
    println!();
}

fn load_project_or_warn(
    cwd: &Path,
    install_hint: &str,
) -> Result<Option<(ProjectContext, crate::config::ProjectConfig)>, HackArenaError> {
    let Some(ctx) = resolve_project_context(cwd)? else {
        println!();
        print_warn(
            "Project",
            "not initialized (missing `./.hackarena/project.json`)",
            &project_config_path(cwd),
        );
        println!(
            "Run `{}` in this directory.",
            cmd_hint::run_cli(install_hint)
        );
        return Ok(None);
    };
    let project = load_project_config(&ctx.workspace_root)?;
    Ok(Some((ctx, project)))
}

fn print_check(label: &str, ok: bool, path: &Path) {
    let colored = std::io::stdout().is_terminal();
    let status = if ok { "OK" } else { "MISSING" };

    if colored {
        let status_s = if ok {
            status.green().to_string()
        } else {
            status.red().to_string()
        };
        if path.as_os_str().is_empty() {
            println!("{label}: {status_s}");
        } else {
            println!("{label}: {status_s} ({})", path.display());
        }
    } else if path.as_os_str().is_empty() {
        println!("{label}: {status}");
    } else {
        println!("{label}: {status} ({})", path.display());
    }
}

fn print_check_value(label: &str, ok: bool, value: &str) {
    let colored = std::io::stdout().is_terminal();
    let status = if ok { "OK" } else { "MISSING" };
    if colored {
        let status_s = if ok {
            status.green().to_string()
        } else {
            status.red().to_string()
        };
        println!("{label}: {status_s} ({value})");
    } else {
        println!("{label}: {status} ({value})");
    }
}

fn print_warn(label: &str, status: &str, path: &Path) {
    let colored = std::io::stdout().is_terminal();
    if colored {
        println!("{label}: {} ({})", status.yellow(), path.display());
    } else {
        println!("{label}: {status} ({})", path.display());
    }
}

fn print_action(action: &str) {
    println!("  -> {action}");
}

fn print_check_with_action(label: &str, ok: bool, path: &Path, action: Option<String>) {
    print_check(label, ok, path);
    if !ok && let Some(action) = action {
        print_action(&format!("run `{action}`"));
    }
}

fn print_standalone_doctor_status(
    manifest: &crate::config::ProjectManifest,
    standalone_dir: &Path,
) {
    let tracked = manifest
        .standalone
        .as_ref()
        .is_some_and(|bundle| bundle.install_dir == PathBuf::from(PROJECT_STANDALONE_DIR));

    if !standalone_dir.exists() {
        print_warn("standalone", "not installed", standalone_dir);
        print_action(&format!(
            "run `{}`",
            cmd_hint::run_cli("install standalone")
        ));
        return;
    }

    if let Some(issue) = standalone_install_layout_issue(standalone_dir) {
        print_warn(
            "standalone",
            &format!("invalid layout: {issue}"),
            standalone_dir,
        );
        let command = if tracked {
            "update standalone"
        } else {
            "install standalone"
        };
        print_action(&format!("run `{}`", cmd_hint::run_cli(command)));
        return;
    }

    if tracked {
        print_check("standalone", true, standalone_dir);
    } else {
        print_warn(
            "standalone",
            "installed on disk, not tracked in manifest",
            standalone_dir,
        );
    }
}

fn print_experimental_wrapper_note(wrapper_id: &str, edition: &str) {
    let status = "experimental";
    if std::io::stdout().is_terminal() {
        println!(
            "wrapper/{wrapper_id}: {} (installed; not supported for official submission in edition {edition})",
            status.yellow(),
        );
    } else {
        println!(
            "wrapper/{wrapper_id}: {status} (installed; not supported for official submission in edition {edition})"
        );
    }
}

fn discover_wrapper_disk_entries(
    cwd: &Path,
    edition: &str,
    manifest: &crate::config::ProjectManifest,
) -> Vec<WrapperDiskEntry> {
    let tracked_ids = manifest.wrappers.keys().cloned().collect::<BTreeSet<_>>();
    discover_installed_wrappers(cwd)
        .into_iter()
        .map(|wrapper_id| {
            let path = cwd.join(PROJECT_WRAPPERS_DIR).join(&wrapper_id);
            let layout_issue =
                wrapper_install_layout_issue(&path).map(|issue| issue.message().to_string());
            WrapperDiskEntry {
                tracked: tracked_ids.contains(&wrapper_id),
                experimental: github_releases::is_experimental_wrapper(edition, &wrapper_id),
                known_for_edition: github_releases::has_wrapper_repo(edition, &wrapper_id),
                id: wrapper_id,
                path,
                layout_issue,
            }
        })
        .collect()
}

fn print_untracked_wrapper_warning(entry: &WrapperDiskEntry) {
    let mut status = if let Some(issue) = entry.layout_issue.as_deref() {
        format!(
            "{}invalid layout on disk, not tracked in manifest: {issue}",
            if entry.experimental {
                "experimental, "
            } else {
                ""
            }
        )
    } else {
        format!(
            "{}installed on disk, not tracked in manifest",
            if entry.experimental {
                "experimental, "
            } else {
                ""
            }
        )
    };
    if !entry.known_for_edition {
        status.push_str("; not configured for this edition");
    }
    print_warn(&format!("wrapper/{}", entry.id), &status, &entry.path);
}

/// Prints active edition and resolved URLs for the current project.
pub async fn status(
    paths: &Paths,
    no_cache: bool,
    prerelease: bool,
    verbose: bool,
) -> Result<(), HackArenaError> {
    let cwd = std::env::current_dir().map_err(HackArenaError::Io)?;
    print_header("hackarena status");
    let Some((ctx, project)) = load_project_or_warn(&cwd, "install")? else {
        return Ok(());
    };
    let workspace_root = &ctx.workspace_root;

    println!("Edition: {}", project.edition);
    println!();

    let project_manifest = load_project_manifest(workspace_root).ok();
    let wrapper_disk_entries = project_manifest
        .as_ref()
        .map(|manifest| discover_wrapper_disk_entries(workspace_root, &project.edition, manifest))
        .unwrap_or_else(|| {
            discover_wrapper_disk_entries(
                workspace_root,
                &project.edition,
                &crate::config::ProjectManifest::default(),
            )
        });

    if verbose {
        println!("Project dir: {}", workspace_root.display());
        print_verbose_runtime(paths, no_cache, prerelease);
        println!();
    }

    let project_manifest = match load_project_manifest(workspace_root) {
        Ok(m) => Some(m),
        Err(_) => None,
    };

    // auth (global)
    let auth_path = paths.bin_dir.join(if cfg!(windows) {
        "ha-auth.exe"
    } else {
        "ha-auth"
    });
    let current_auth_sha = project_manifest
        .as_ref()
        .and_then(|m| m.auth.as_ref())
        .and_then(|a| a.sha256.as_deref())
        .map(|s| s.to_string())
        .or_else(|| sha256_file_hex(&auth_path).ok());
    let current_auth_version = current_auth_version_from_binary(&auth_path);
    let latest_auth = github_releases::latest_auth_from_releases(
        paths,
        &project.edition,
        no_cache,
        prerelease,
        current_auth_version.as_deref(),
        None,
    )
    .await;
    let latest_auth_version = latest_auth
        .as_ref()
        .ok()
        .and_then(|(url, _)| auth_version_from_asset_url(url));
    match (current_auth_sha.as_deref(), latest_auth) {
        (None, _) => println!("auth: unknown"),
        (Some(_), Err(_)) => println!("auth: unknown (cannot check latest)"),
        (Some(current), Ok((_url, latest_sha))) => {
            if current.eq_ignore_ascii_case(&latest_sha) {
                if let Some(version) = current_auth_version
                    .as_deref()
                    .or(latest_auth_version.as_deref())
                {
                    println!("auth: up to date ({})", format_version(version));
                } else {
                    println!("auth: up to date");
                }
            } else {
                println!(
                    "auth: update available ({} -> {})",
                    format_version_opt(current_auth_version.as_deref()),
                    format_version_opt(latest_auth_version.as_deref())
                );
            }
        }
    }

    // backend (project-local)
    let current_backend = project_manifest.as_ref().and_then(|m| m.backend.as_ref());
    let current_backend_version =
        current_backend.and_then(|b| backend_version_from_asset_url(&b.url));
    let latest_backend = github_releases::latest_backend_from_releases(
        paths,
        &project.edition,
        no_cache,
        prerelease,
        current_backend_version.as_deref(),
        None,
    )
    .await;
    let latest_backend_version = latest_backend
        .as_ref()
        .ok()
        .and_then(|b| b.as_ref())
        .and_then(|b| backend_version_from_asset_url(&b.url));
    match (current_backend, latest_backend) {
        (_, Err(_)) => println!("backend: unknown (cannot check latest)"),
        (None, Ok(None)) if github_releases::has_backend_repo(&project.edition) => {
            println!("backend: no release yet")
        }
        (None, Ok(None)) => println!("backend: n/a (not configured)"),
        (None, Ok(Some(_))) => println!("backend: not installed"),
        (Some(_), Ok(None)) if github_releases::has_backend_repo(&project.edition) => {
            println!("backend: installed, but no release available now")
        }
        (Some(_), Ok(None)) => println!("backend: n/a (not configured)"),
        (Some(current), Ok(Some(latest))) => {
            let current_sha = current.sha256.as_deref();
            let latest_sha = latest.sha256.as_deref();
            if current_sha == latest_sha && current.url == latest.url {
                if let Some(version) = current_backend_version
                    .as_deref()
                    .or(latest_backend_version.as_deref())
                {
                    println!("backend: up to date ({})", format_version(version));
                } else {
                    println!("backend: up to date");
                }
            } else {
                println!(
                    "backend: update available ({} -> {})",
                    format_version_opt(current_backend_version.as_deref()),
                    format_version_opt(latest_backend_version.as_deref())
                );
            }
        }
    }

    // standalone (project-local, optional)
    let standalone_dir = workspace_root.join(PROJECT_STANDALONE_DIR);
    let current_standalone = project_manifest
        .as_ref()
        .and_then(|m| m.standalone.as_ref());
    let standalone_tracked = current_standalone
        .is_some_and(|bundle| bundle.install_dir == PathBuf::from(PROJECT_STANDALONE_DIR));
    if !standalone_tracked && standalone_dir.exists() {
        if let Some(issue) = standalone_install_layout_issue(&standalone_dir) {
            println!("standalone: invalid layout on disk, not tracked in manifest: {issue}");
        } else {
            println!("standalone: installed on disk, not tracked in manifest");
        }
    } else if standalone_tracked && !standalone_dir.exists() {
        println!("standalone: not installed");
    } else if standalone_tracked
        && standalone_dir.exists()
        && let Some(issue) = standalone_install_layout_issue(&standalone_dir)
    {
        println!("standalone: invalid layout on disk: {issue}");
    } else {
        let current_standalone_version =
            current_standalone.and_then(|bundle| standalone_version_from_asset_url(&bundle.url));
        let latest_standalone = github_releases::latest_standalone_from_releases(
            paths,
            &project.edition,
            no_cache,
            prerelease,
            current_standalone_version.as_deref(),
            None,
        )
        .await;
        let latest_standalone_version = latest_standalone
            .as_ref()
            .ok()
            .and_then(|bundle| bundle.as_ref())
            .and_then(|bundle| standalone_version_from_asset_url(&bundle.url));
        match (current_standalone, latest_standalone) {
            (_, Err(_)) => println!("standalone: unknown (cannot check latest)"),
            (None, Ok(None)) => println!("standalone: not installed"),
            (None, Ok(Some(_))) => println!("standalone: not installed"),
            (Some(_), Ok(None)) => println!("standalone: installed, but no release available now"),
            (Some(current), Ok(Some(latest))) => {
                let current_sha = current.sha256.as_deref();
                let latest_sha = latest.sha256.as_deref();
                if current_sha == latest_sha && current.url == latest.url {
                    if let Some(version) = current_standalone_version
                        .as_deref()
                        .or(latest_standalone_version.as_deref())
                    {
                        println!("standalone: up to date ({})", format_version(version));
                    } else {
                        println!("standalone: up to date");
                    }
                } else {
                    println!(
                        "standalone: update available ({} -> {})",
                        format_version_opt(current_standalone_version.as_deref()),
                        format_version_opt(latest_standalone_version.as_deref())
                    );
                }
            }
        }
    }

    // wrapper (project-local, optional)
    let installed_wrappers = project_manifest
        .as_ref()
        .map(|m| m.wrappers.clone())
        .unwrap_or_default();
    let untracked_wrappers = wrapper_disk_entries
        .iter()
        .filter(|entry| entry.is_untracked())
        .collect::<Vec<_>>();
    let configured_wrapper_ids = github_releases::wrapper_ids_for_edition(&project.edition);
    if configured_wrapper_ids.is_empty()
        && installed_wrappers.is_empty()
        && untracked_wrappers.is_empty()
    {
        println!("wrapper: n/a (no wrappers configured)");
        return Ok(());
    }

    for wrapper_id in configured_wrapper_ids {
        let instances = installed_wrappers
            .iter()
            .filter(|(instance_id, _)| github_releases::wrapper_base_id(instance_id) == wrapper_id)
            .collect::<Vec<_>>();
        let untracked_instances = untracked_wrappers
            .iter()
            .filter(|entry| github_releases::wrapper_base_id(&entry.id) == wrapper_id)
            .collect::<Vec<_>>();
        let latest = github_releases::latest_wrapper_from_releases(
            paths,
            &project.edition,
            wrapper_id,
            no_cache,
            prerelease,
            None,
            None,
        )
        .await;
        let latest_wrapper_version = latest
            .as_ref()
            .ok()
            .and_then(|w| w.as_ref())
            .and_then(|w| wrapper_version_from_asset_url(wrapper_id, &w.url));

        match latest {
            Err(_) => {
                if instances.is_empty() {
                    if untracked_instances.is_empty() {
                        println!("wrapper/{wrapper_id}: unknown (cannot check latest)");
                    }
                } else {
                    for (instance_id, _current) in &instances {
                        println!("wrapper/{instance_id}: unknown (cannot check latest)");
                    }
                }
            }
            Ok(None) => {
                if instances.is_empty() {
                    if untracked_instances.is_empty() {
                        println!("wrapper/{wrapper_id}: no release yet");
                    }
                } else {
                    for (instance_id, _current) in &instances {
                        println!("wrapper/{instance_id}: installed, but no release available now");
                    }
                }
            }
            Ok(Some(latest_bundle)) => {
                if instances.is_empty() {
                    if untracked_instances.is_empty() {
                        println!("wrapper/{wrapper_id}: not installed");
                    }
                    continue;
                }
                for (instance_id, current) in &instances {
                    let current_wrapper_version =
                        wrapper_version_from_asset_url(wrapper_id, &current.url);
                    let current_sha = current.sha256.as_deref();
                    let latest_sha = latest_bundle.sha256.as_deref();
                    if current_sha == latest_sha && current.url == latest_bundle.url {
                        if let Some(version) = current_wrapper_version
                            .as_deref()
                            .or(latest_wrapper_version.as_deref())
                        {
                            println!(
                                "wrapper/{instance_id}: up to date ({})",
                                format_version(version)
                            );
                        } else {
                            println!("wrapper/{instance_id}: up to date");
                        }
                    } else {
                        println!(
                            "wrapper/{instance_id}: update available ({} -> {})",
                            format_version_opt(current_wrapper_version.as_deref()),
                            format_version_opt(latest_wrapper_version.as_deref())
                        );
                    }
                }
            }
        }
    }

    for (wrapper_id, current) in installed_wrappers.iter().filter(|(wrapper_id, _)| {
        github_releases::is_experimental_wrapper(&project.edition, wrapper_id)
    }) {
        let current_wrapper_version = wrapper_version_from_asset_url(
            github_releases::wrapper_base_id(wrapper_id),
            &current.url,
        );
        let latest = github_releases::latest_wrapper_from_releases(
            paths,
            &project.edition,
            wrapper_id,
            no_cache,
            prerelease,
            current_wrapper_version.as_deref(),
            None,
        )
        .await;
        let latest_wrapper_version = latest
            .as_ref()
            .ok()
            .and_then(|bundle| bundle.as_ref())
            .and_then(|bundle| {
                wrapper_version_from_asset_url(
                    github_releases::wrapper_base_id(wrapper_id),
                    &bundle.url,
                )
            });
        match latest {
            Err(_) => println!("wrapper/{wrapper_id}: experimental, unknown (cannot check latest)"),
            Ok(None) => println!(
                "wrapper/{wrapper_id}: experimental, installed but no release available now"
            ),
            Ok(Some(latest_bundle)) => {
                let current_sha = current.sha256.as_deref();
                let latest_sha = latest_bundle.sha256.as_deref();
                if current_sha == latest_sha && current.url == latest_bundle.url {
                    if let Some(version) = current_wrapper_version
                        .as_deref()
                        .or(latest_wrapper_version.as_deref())
                    {
                        println!(
                            "wrapper/{wrapper_id}: experimental, up to date ({})",
                            format_version(version)
                        );
                    } else {
                        println!("wrapper/{wrapper_id}: experimental, up to date");
                    }
                } else {
                    println!(
                        "wrapper/{wrapper_id}: experimental, update available ({} -> {})",
                        format_version_opt(current_wrapper_version.as_deref()),
                        format_version_opt(latest_wrapper_version.as_deref())
                    );
                }
            }
        }
    }

    for (wrapper_id, _current) in installed_wrappers {
        if !github_releases::has_wrapper_repo(&project.edition, &wrapper_id) {
            println!("wrapper/{wrapper_id}: installed (not configured for this edition)");
        }
    }

    for entry in untracked_wrappers {
        print_untracked_wrapper_status(entry);
    }

    Ok(())
}

fn print_untracked_wrapper_status(entry: &WrapperDiskEntry) {
    let experimental_prefix = if entry.experimental {
        "experimental, "
    } else {
        ""
    };
    match entry.layout_issue.as_deref() {
        Some(issue) => {
            let suffix = if entry.known_for_edition {
                ""
            } else {
                "; not configured for this edition"
            };
            println!(
                "wrapper/{}: {}invalid layout on disk, not tracked in manifest: {}{}",
                entry.id, experimental_prefix, issue, suffix
            );
        }
        None => {
            let suffix = if entry.known_for_edition {
                ""
            } else {
                "; not configured for this edition"
            };
            println!(
                "wrapper/{}: {}installed on disk, not tracked in manifest{}",
                entry.id, experimental_prefix, suffix
            );
        }
    }
}

fn print_verbose_runtime(paths: &Paths, no_cache: bool, prerelease: bool) {
    let cache_mode = if no_cache {
        "disabled (`--no-cache`)"
    } else {
        "enabled"
    };
    let channel = if prerelease {
        "stable+prerelease (`--prerelease`)"
    } else {
        "stable-only"
    };
    let token = github_auth::github_token_source(paths)
        .ok()
        .flatten()
        .map(|value| value.as_str())
        .unwrap_or("none");
    println!("Verbose: cache: {cache_mode}");
    println!("Verbose: release channel: {channel}");
    println!("Verbose: GitHub token source: {token}");
    if let Ok(Some(linux_libc)) = github_releases::linux_libc_verbose_summary(None) {
        println!("Verbose: linux libc: {linux_libc}");
    }
}

fn print_github_auth_status(paths: &Paths) -> Result<(), HackArenaError> {
    match github_auth::github_token_source(paths)? {
        Some(source) => {
            println!("GitHub auth: {}", source.as_str());
        }
        None => {
            println!("GitHub auth: none");
            print_action(&format!(
                "run `{}` or set GH_TOKEN/GITHUB_TOKEN to avoid anonymous rate limits",
                cmd_hint::run_cli("github login")
            ));
        }
    }
    Ok(())
}

fn format_version(version: &str) -> String {
    let normalized = normalize_version(version);
    if normalized.is_empty() {
        "unknown".to_string()
    } else {
        format!("v{normalized}")
    }
}

fn format_version_opt(version: Option<&str>) -> String {
    match version {
        Some(v) => format_version(v),
        None => "unknown".to_string(),
    }
}

fn normalize_version(version: &str) -> String {
    github_releases::normalize_version_string(version)
}

fn current_auth_version_from_binary(auth_path: &Path) -> Option<String> {
    if !auth_path.exists() {
        return None;
    }

    for flag in ["-V", "--version"] {
        let output = match std::process::Command::new(auth_path).arg(flag).output() {
            Ok(output) => output,
            Err(_) => continue,
        };
        if !output.status.success() {
            continue;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Some(version) = parse_version_from_cli_output(&stdout) {
            return Some(version);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if let Some(version) = parse_version_from_cli_output(&stderr) {
            return Some(version);
        }
    }
    None
}

fn parse_version_from_cli_output(output: &str) -> Option<String> {
    for token in output.split_whitespace().rev() {
        if !looks_like_version_token(token) {
            continue;
        }
        let normalized = normalize_version(token);
        if !normalized.is_empty() {
            return Some(normalized);
        }
    }
    None
}

fn looks_like_version_token(token: &str) -> bool {
    let trimmed = token.trim_matches(|c: char| c == ',' || c == ';' || c == ')' || c == '(');
    trimmed.chars().any(|c| c.is_ascii_digit())
        && trimmed.chars().all(|c| {
            c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '+' || c == '_' || c == 'v'
        })
}

fn auth_version_from_asset_url(url: &str) -> Option<String> {
    github_releases::auth_version_from_asset_url(url)
}

fn backend_version_from_asset_url(url: &str) -> Option<String> {
    github_releases::backend_version_from_asset_url(url)
}

fn standalone_version_from_asset_url(url: &str) -> Option<String> {
    github_releases::standalone_version_from_asset_url(url)
}

fn wrapper_version_from_asset_url(wrapper_id: &str, url: &str) -> Option<String> {
    github_releases::wrapper_version_from_asset_url(wrapper_id, url)
}

#[cfg(test)]
mod tests {
    use super::{
        auth_version_from_asset_url, backend_version_from_asset_url, format_version_opt,
        parse_version_from_cli_output, standalone_version_from_asset_url,
        wrapper_version_from_asset_url,
    };

    #[test]
    fn parses_versions_from_asset_urls() {
        let auth = "https://api.github.com/repos/INIT-SGGW/HackArena-Auth-Cli/releases/assets/1?asset_name=ha-auth-v0.2.0-x86_64-pc-windows-msvc.exe";
        let backend = "https://api.github.com/repos/INIT-SGGW/HackArena3.0-Backend/releases/assets/2?asset_name=ha3-backend-local-x86_64-pc-windows-msvc-v0.1.0-beta.1.zip";
        let standalone = "https://api.github.com/repos/INIT-SGGW/HackArena3.0-Backend/releases/assets/4?asset_name=ha3-standalone-x86_64-pc-windows-msvc-v0.2.0-beta.14.zip";
        let wrapper = "https://api.github.com/repos/INIT-SGGW/HackArena3.0-ApiWrapper-Python/releases/assets/3?asset_name=wrapper-python-v0.1.0b3.zip";

        assert_eq!(auth_version_from_asset_url(auth).as_deref(), Some("0.2.0"));
        assert_eq!(
            backend_version_from_asset_url(backend).as_deref(),
            Some("0.1.0-beta.1")
        );
        assert_eq!(
            standalone_version_from_asset_url(standalone).as_deref(),
            Some("0.2.0-beta.14")
        );
        assert_eq!(
            wrapper_version_from_asset_url("python", wrapper).as_deref(),
            Some("0.1.0b3")
        );
    }

    #[test]
    fn parses_auth_cli_output() {
        assert_eq!(
            parse_version_from_cli_output("ha-auth 0.2.0").as_deref(),
            Some("0.2.0")
        );
        assert_eq!(
            parse_version_from_cli_output("ha-auth v0.3.0-beta.1").as_deref(),
            Some("0.3.0-beta.1")
        );
    }

    #[test]
    fn formats_unknown_version() {
        assert_eq!(format_version_opt(None), "unknown");
    }
}
