use crate::cmd_hint;
use crate::config::{
    Paths, is_project_dir, load_project_config, load_project_manifest, project_config_path,
    project_manifest_path, validate_edition,
};
use crate::download::sha256_file_hex;
use crate::error::HackArenaError;
use crate::github_releases;
use owo_colors::OwoColorize;
use std::io::IsTerminal;
use std::path::Path;

/// Prints diagnostics for the current installation.
pub async fn doctor(
    paths: &Paths,
    no_cache: bool,
    prerelease: bool,
    verbose: bool,
) -> Result<(), HackArenaError> {
    let cwd = std::env::current_dir().map_err(HackArenaError::Io)?;
    print_header("hackarena doctor");
    println!("Project dir: {}", cwd.display());
    let Some(project) = load_project_or_warn(&cwd, "install")? else {
        return Ok(());
    };
    let project_path = project_config_path(&cwd);
    print_check("project.json", project_path.exists(), &project_path);

    let edition_ok = validate_edition(&project.edition).is_ok();
    print_check_value("edition", edition_ok, &project.edition);
    println!();

    let manifest_path = project_manifest_path(&cwd);
    print_check("manifest.json", manifest_path.exists(), &manifest_path);

    let _config = load_effective_config(paths, &project, no_cache, prerelease).await?;
    println!("Artifact source: GitHub Releases");
    if verbose {
        print_verbose_runtime(no_cache, prerelease);
    }
    println!();

    let auth_path = paths.bin_dir.join(if cfg!(windows) {
        "ha-auth.exe"
    } else {
        "ha-auth"
    });
    print_check("global ha-auth", auth_path.exists(), &auth_path);

    let backend_dir = cwd.join(&project.backend_dir);
    print_check("backend", backend_dir.exists(), &backend_dir);

    print_check(
        "wrappers/",
        cwd.join("wrappers").exists(),
        &cwd.join("wrappers"),
    );

    Ok(())
}

fn print_header(title: &str) {
    println!("{title}");
    println!();
}

fn load_project_or_warn(
    cwd: &Path,
    install_hint: &str,
) -> Result<Option<crate::config::ProjectConfig>, HackArenaError> {
    if !is_project_dir(cwd) {
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
    }
    Ok(Some(load_project_config(cwd)?))
}

async fn load_effective_config(
    paths: &Paths,
    project: &crate::config::ProjectConfig,
    no_cache: bool,
    prerelease: bool,
) -> Result<crate::config::EditionConfig, HackArenaError> {
    github_releases::load_edition_config_from_cache(paths, &project.edition, no_cache, prerelease)
        .await
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

/// Prints active edition and resolved URLs for the current project.
pub async fn status(
    paths: &Paths,
    no_cache: bool,
    prerelease: bool,
    verbose: bool,
) -> Result<(), HackArenaError> {
    let cwd = std::env::current_dir().map_err(HackArenaError::Io)?;
    print_header("hackarena status");
    let Some(project) = load_project_or_warn(&cwd, "install")? else {
        return Ok(());
    };
    let config = load_effective_config(paths, &project, no_cache, prerelease).await?;

    println!("Project dir: {}", cwd.display());
    println!("Edition: {}", project.edition);
    println!();

    let wrappers_dir = cwd.join("wrappers");
    println!("Wrappers dir: {}", wrappers_dir.display());
    println!("Global ha-auth: {}", paths.bin_dir.display());
    println!();

    println!("Auth URL:    {}", config.auth_artifact.filename);

    match config.backend.as_ref() {
        None => {
            println!("Backend:     <not configured for this edition>");
        }
        Some(backend) => {
            match &backend.source {
                crate::config::BackendSource::Url { url } => println!("Backend URL: {url}"),
            }
            println!("Backend dir: {}", cwd.join(&project.backend_dir).display());
        }
    }

    let project_manifest = load_project_manifest(&cwd).ok();
    if let Some(m) = project_manifest.as_ref() {
        if !m.wrappers.is_empty() {
            println!(
                "Wrappers installed: {}",
                m.wrappers.keys().cloned().collect::<Vec<_>>().join(", ")
            );
        }
    }

    if verbose {
        println!();
        print_verbose_runtime(no_cache, prerelease);
        println!(
            "Update check uses GitHub Releases metadata. For private repos, set GH_TOKEN or GITHUB_TOKEN."
        );
    }

    let project_manifest = match load_project_manifest(&cwd) {
        Ok(m) => Some(m),
        Err(_) => None,
    };

    // auth (global)
    let auth_path = paths.bin_dir.join(&config.bin_name_auth);
    let current_auth_sha = project_manifest
        .as_ref()
        .and_then(|m| m.auth.as_ref())
        .and_then(|a| a.sha256.as_deref())
        .map(|s| s.to_string())
        .or_else(|| sha256_file_hex(&auth_path).ok());
    let latest_auth =
        github_releases::latest_auth_from_releases(paths, &project.edition, no_cache, prerelease)
            .await;
    let current_auth_version = current_auth_version_from_binary(&auth_path);
    let latest_auth_version = latest_auth
        .as_ref()
        .ok()
        .and_then(|(url, _)| auth_version_from_asset_url(url));
    match (current_auth_sha.as_deref(), latest_auth) {
        (None, _) => println!(
            "auth: unknown (missing local sha256; run `{}`)",
            cmd_hint::run_cli("install auth")
        ),
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
                    "auth: update available ({} -> {}; run `{}`)",
                    format_version_opt(current_auth_version.as_deref()),
                    format_version_opt(latest_auth_version.as_deref()),
                    cmd_hint::run_cli("update auth")
                );
            }
        }
    }

    // backend (project-local)
    let latest_backend = github_releases::latest_backend_from_releases(
        paths,
        &project.edition,
        no_cache,
        prerelease,
    )
    .await;
    let current_backend = project_manifest.as_ref().and_then(|m| m.backend.as_ref());
    let current_backend_version =
        current_backend.and_then(|b| backend_version_from_asset_url(&b.url));
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
        (None, Ok(Some(_))) => println!(
            "backend: not installed (run `{}`)",
            cmd_hint::run_cli("install backend")
        ),
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
                    "backend: update available ({} -> {}; run `{}`)",
                    format_version_opt(current_backend_version.as_deref()),
                    format_version_opt(latest_backend_version.as_deref()),
                    cmd_hint::run_cli("update backend")
                );
            }
        }
    }

    // wrapper (project-local, optional)
    let installed_wrappers = project_manifest
        .as_ref()
        .map(|m| m.wrappers.clone())
        .unwrap_or_default();
    let configured_wrapper_ids = github_releases::wrapper_ids_for_edition(&project.edition);
    if configured_wrapper_ids.is_empty() && installed_wrappers.is_empty() {
        println!("wrapper: n/a (no wrappers configured)");
        return Ok(());
    }

    for wrapper_id in configured_wrapper_ids {
        let instances = installed_wrappers
            .iter()
            .filter(|(instance_id, _)| github_releases::wrapper_base_id(instance_id) == wrapper_id)
            .collect::<Vec<_>>();
        let latest = github_releases::latest_wrapper_from_releases(
            paths,
            &project.edition,
            wrapper_id,
            no_cache,
            prerelease,
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
                    println!("wrapper/{wrapper_id}: unknown (cannot check latest)");
                } else {
                    for (instance_id, _current) in &instances {
                        println!("wrapper/{instance_id}: unknown (cannot check latest)");
                    }
                }
            }
            Ok(None) => {
                if instances.is_empty() {
                    println!("wrapper/{wrapper_id}: no release yet");
                } else {
                    for (instance_id, _current) in &instances {
                        println!("wrapper/{instance_id}: installed, but no release available now");
                    }
                }
            }
            Ok(Some(latest_bundle)) => {
                if instances.is_empty() {
                    println!(
                        "wrapper/{wrapper_id}: not installed (run `{}`)",
                        cmd_hint::run_cli(&format!("install wrapper {wrapper_id}"))
                    );
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
                            "wrapper/{instance_id}: update available ({} -> {}; run `{}`)",
                            format_version_opt(current_wrapper_version.as_deref()),
                            format_version_opt(latest_wrapper_version.as_deref()),
                            cmd_hint::run_cli(&format!("update wrapper {instance_id}"))
                        );
                    }
                }
            }
        }
    }

    for (wrapper_id, _current) in installed_wrappers {
        if !github_releases::has_wrapper_repo(&project.edition, &wrapper_id) {
            println!("wrapper/{wrapper_id}: installed (not configured for this edition)");
        }
    }

    Ok(())
}

fn print_verbose_runtime(no_cache: bool, prerelease: bool) {
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
    let token = if github_token_present() {
        "set"
    } else {
        "not set"
    };
    println!("Verbose: cache: {cache_mode}");
    println!("Verbose: release channel: {channel}");
    println!("Verbose: GH_TOKEN/GITHUB_TOKEN: {token}");
}

fn github_token_present() -> bool {
    for key in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(v) = std::env::var(key)
            && !v.trim().is_empty()
        {
            return true;
        }
    }
    false
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
    version
        .trim()
        .trim_start_matches(['v', 'V'])
        .trim()
        .to_string()
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
    let asset = asset_name_from_url(url)?;
    let stem = strip_asset_extension(&asset);
    let prefix = "ha-auth-v";
    if !stem.to_ascii_lowercase().starts_with(prefix) {
        return None;
    }
    let rest = &stem[prefix.len()..];
    let version = strip_known_triple_suffix(rest);
    let normalized = normalize_version(version);
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn backend_version_from_asset_url(url: &str) -> Option<String> {
    let asset = asset_name_from_url(url)?;
    let stem = strip_asset_extension(&asset);
    let stem_lower = stem.to_ascii_lowercase();
    if !stem_lower.contains("-backend-local-") {
        return None;
    }
    let idx = stem_lower.rfind("-v")?;
    let version = &stem[idx + 2..];
    let normalized = normalize_version(version);
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn wrapper_version_from_asset_url(wrapper_id: &str, url: &str) -> Option<String> {
    let asset = asset_name_from_url(url)?;
    let stem = strip_asset_extension(&asset);
    let prefix = format!("wrapper-{}-v", wrapper_id.to_ascii_lowercase());
    if !stem.to_ascii_lowercase().starts_with(&prefix) {
        return None;
    }
    let rest = &stem[prefix.len()..];
    let version = strip_known_triple_suffix(rest);
    let normalized = normalize_version(version);
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn asset_name_from_url(url: &str) -> Option<String> {
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

fn strip_asset_extension(asset: &str) -> &str {
    if let Some(stem) = asset.strip_suffix(".tar.gz") {
        return stem;
    }
    if let Some(stem) = asset.strip_suffix(".zip") {
        return stem;
    }
    if let Some(stem) = asset.strip_suffix(".exe") {
        return stem;
    }
    if let Some(stem) = asset.strip_suffix(".whl") {
        return stem;
    }
    asset
}

fn strip_known_triple_suffix(value: &str) -> &str {
    const TRIPLES: &[&str] = &[
        "x86_64-pc-windows-msvc",
        "aarch64-pc-windows-msvc",
        "x86_64-unknown-linux-musl",
        "aarch64-unknown-linux-musl",
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
    ];

    let value_lower = value.to_ascii_lowercase();
    for triple in TRIPLES {
        let suffix = format!("-{triple}");
        if value_lower.ends_with(&suffix) {
            return &value[..value.len().saturating_sub(suffix.len())];
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::{
        auth_version_from_asset_url, backend_version_from_asset_url, format_version_opt,
        parse_version_from_cli_output, wrapper_version_from_asset_url,
    };

    #[test]
    fn parses_versions_from_asset_urls() {
        let auth = "https://api.github.com/repos/INIT-SGGW/HackArena-Auth-Cli/releases/assets/1?asset_name=ha-auth-v0.2.0-x86_64-pc-windows-msvc.exe";
        let backend = "https://api.github.com/repos/INIT-SGGW/HackArena3.0-Backend/releases/assets/2?asset_name=ha3-backend-local-x86_64-pc-windows-msvc-v0.1.0-beta.1.zip";
        let wrapper = "https://api.github.com/repos/INIT-SGGW/HackArena3.0-ApiWrapper-Python/releases/assets/3?asset_name=wrapper-python-v0.1.0b3.zip";

        assert_eq!(auth_version_from_asset_url(auth).as_deref(), Some("0.2.0"));
        assert_eq!(
            backend_version_from_asset_url(backend).as_deref(),
            Some("0.1.0-beta.1")
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
