use crate::config::{
    ArtifactSpec, BackendConfig, BackendSource, EditionConfig, Paths, ProjectInstalledBundle,
    WrapperSpec, ensure_dir,
};
use crate::constants::{PROJECT_BACKEND_DIR, PROJECT_WRAPPERS_DIR};
use crate::error::HackArenaError;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

const AUTH_REPO: &str = "INIT-SGGW/HackArena-Auth-Cli";
const BACKEND_REPO_EDITION_3: &str = "INIT-SGGW/HackArena3.0-Backend";
const WRAPPERS_EDITION_3: &[(&str, &str)] = &[
    ("python", "INIT-SGGW/HackArena3.0-ApiWrapper-Python"),
    ("csharp", "INIT-SGGW/HackArena3.0-ApiWrapper-CSharp"),
    ("cpp", "INIT-SGGW/HackArena3.0-ApiWrapper-Cpp"),
    ("typescript", "INIT-SGGW/HackArena3.0-ApiWrapper-TypeScript"),
];
const CHECKSUMS_ASSET_NAME: &str = "SHA256SUMS.txt";
const RELEASE_CACHE_TTL: Duration = Duration::from_secs(300);
const LINUX_LIBC_ENV: &str = "HACKARENA_LINUX_LIBC";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxLibcMode {
    Auto,
    Gnu,
    Musl,
}

impl LinuxLibcMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Gnu => "gnu",
            Self::Musl => "musl",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        let lowered = value.trim().to_ascii_lowercase();
        match lowered.as_str() {
            "auto" => Some(Self::Auto),
            "gnu" => Some(Self::Gnu),
            "musl" => Some(Self::Musl),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
struct TargetTripleResolution {
    triples: Vec<&'static str>,
    linux_details: Option<LinuxModeDetails>,
}

#[derive(Debug, Clone)]
struct LinuxModeDetails {
    mode: LinuxLibcMode,
    source: &'static str,
    order_label: &'static str,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    name: String,
    draft: bool,
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GithubAsset {
    name: String,
    url: String,
    browser_download_url: String,
}

#[derive(Debug, Clone)]
struct ResolvedReleaseAsset {
    name: String,
    url: String,
    sha256: String,
}

#[derive(Debug, Clone, Copy)]
struct EditionRepos {
    auth_repo: &'static str,
    backend_repo: Option<&'static str>,
    wrappers: &'static [(&'static str, &'static str)],
}

#[derive(Debug, Clone, Copy)]
enum ComponentSelector<'a> {
    Auth,
    Backend,
    Wrapper(&'a str),
}

pub fn wrapper_base_id(wrapper_id: &str) -> &str {
    let Some((base, suffix)) = wrapper_id.rsplit_once('_') else {
        return wrapper_id;
    };
    if base.is_empty() || suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_digit()) {
        return wrapper_id;
    }
    base
}

pub fn has_backend_repo(edition: &str) -> bool {
    repos_for_edition(edition)
        .and_then(|r| r.backend_repo)
        .is_some()
}

pub fn wrapper_ids_for_edition(edition: &str) -> Vec<&'static str> {
    repos_for_edition(edition)
        .map(|r| r.wrappers.iter().map(|(id, _)| *id).collect())
        .unwrap_or_default()
}

pub fn has_wrapper_repo(edition: &str, wrapper_id: &str) -> bool {
    let base = wrapper_base_id(wrapper_id);
    repos_for_edition(edition)
        .map(|r| r.wrappers.iter().any(|(id, _)| *id == base))
        .unwrap_or(false)
}

pub async fn load_edition_config_from_cache(
    paths: &Paths,
    edition: &str,
    no_cache: bool,
    prerelease: bool,
    linux_libc_override: Option<LinuxLibcMode>,
) -> Result<EditionConfig, HackArenaError> {
    let repos = repos_for_edition(edition).ok_or_else(|| {
        HackArenaError::msg(format!(
            "GitHub Releases mapping is not configured for edition `{edition}`"
        ))
    })?;
    let target = current_target_triples(linux_libc_override)?;
    let use_cache = !no_cache;
    let allow_prerelease = prerelease;

    let auth = resolve_required_component(
        paths,
        repos.auth_repo,
        ComponentSelector::Auth,
        &target,
        use_cache,
        allow_prerelease,
    )
    .await?;

    let backend = match repos.backend_repo {
        Some(repo) => resolve_optional_component(
            paths,
            repo,
            ComponentSelector::Backend,
            &target,
            use_cache,
            allow_prerelease,
        )
        .await?
        .map(|asset| BackendConfig {
            source: BackendSource::Url { url: asset.url },
            sha256: asset.sha256,
        }),
        None => None,
    };

    let mut wrappers = Vec::new();
    for (wrapper_id, repo) in repos.wrappers {
        if let Some(asset) = resolve_optional_component(
            paths,
            repo,
            ComponentSelector::Wrapper(wrapper_id),
            &target,
            use_cache,
            allow_prerelease,
        )
        .await?
        {
            wrappers.push(WrapperSpec {
                id: (*wrapper_id).to_string(),
                filename: asset.url,
                sha256: asset.sha256,
            });
        }
    }

    let default_wrapper_id = repos
        .wrappers
        .first()
        .map(|(id, _)| (*id).to_string())
        .unwrap_or_else(|| "default".to_string());

    Ok(EditionConfig {
        edition: edition.to_string(),
        backend,
        auth_artifact: ArtifactSpec {
            filename: auth.url,
            sha256: auth.sha256,
        },
        wrappers,
        default_wrapper_id,
        bin_name_auth: infer_bin_name_auth(&auth.name),
    })
}

pub async fn latest_auth_from_releases(
    paths: &Paths,
    edition: &str,
    no_cache: bool,
    prerelease: bool,
    linux_libc_override: Option<LinuxLibcMode>,
) -> Result<(String, String), HackArenaError> {
    let cfg =
        load_edition_config_from_cache(paths, edition, no_cache, prerelease, linux_libc_override)
            .await?;
    Ok((cfg.auth_artifact.filename, cfg.auth_artifact.sha256))
}

pub async fn latest_backend_from_releases(
    paths: &Paths,
    edition: &str,
    no_cache: bool,
    prerelease: bool,
    linux_libc_override: Option<LinuxLibcMode>,
) -> Result<Option<ProjectInstalledBundle>, HackArenaError> {
    let cfg =
        load_edition_config_from_cache(paths, edition, no_cache, prerelease, linux_libc_override)
            .await?;
    let Some(backend) = cfg.backend.as_ref() else {
        return Ok(None);
    };
    let BackendSource::Url { url } = &backend.source;
    Ok(Some(ProjectInstalledBundle {
        url: url.clone(),
        install_dir: PathBuf::from(PROJECT_BACKEND_DIR),
        sha256: Some(backend.sha256.clone()),
        installed_at_unix: None,
        files: vec![],
    }))
}

pub async fn latest_wrapper_from_releases(
    paths: &Paths,
    edition: &str,
    wrapper_id: &str,
    no_cache: bool,
    prerelease: bool,
    linux_libc_override: Option<LinuxLibcMode>,
) -> Result<Option<ProjectInstalledBundle>, HackArenaError> {
    let cfg =
        load_edition_config_from_cache(paths, edition, no_cache, prerelease, linux_libc_override)
            .await?;
    let base_id = wrapper_base_id(wrapper_id);
    let Some(wrapper) = cfg.wrapper(base_id) else {
        return Ok(None);
    };
    Ok(Some(ProjectInstalledBundle {
        url: wrapper.filename.clone(),
        install_dir: PathBuf::from(PROJECT_WRAPPERS_DIR).join(wrapper_id),
        sha256: Some(wrapper.sha256.clone()),
        installed_at_unix: None,
        files: vec![],
    }))
}

pub async fn wrapper_from_release_tag(
    paths: &Paths,
    edition: &str,
    wrapper_id: &str,
    release_tag: &str,
    no_cache: bool,
    linux_libc_override: Option<LinuxLibcMode>,
) -> Result<Option<ProjectInstalledBundle>, HackArenaError> {
    let Some(repo) = wrapper_repo_for_edition(edition, wrapper_id) else {
        return Ok(None);
    };
    let base_id = wrapper_base_id(wrapper_id);
    let target = current_target_triples(linux_libc_override)?;
    let use_cache = !no_cache;
    let Some(release) = release_by_tag(paths, repo, release_tag, use_cache).await? else {
        return Ok(None);
    };
    let asset = resolve_component_from_release(
        repo,
        &release,
        ComponentSelector::Wrapper(base_id),
        &target,
    )
    .await?;
    Ok(Some(ProjectInstalledBundle {
        url: asset.url,
        install_dir: PathBuf::from(PROJECT_WRAPPERS_DIR).join(wrapper_id),
        sha256: Some(asset.sha256),
        installed_at_unix: None,
        files: vec![],
    }))
}

pub async fn latest_wrapper_python_wheel_from_releases(
    paths: &Paths,
    edition: &str,
    wrapper_id: &str,
    no_cache: bool,
    prerelease: bool,
    linux_libc_override: Option<LinuxLibcMode>,
) -> Result<Option<ArtifactSpec>, HackArenaError> {
    let Some(repo) = wrapper_repo_for_edition(edition, wrapper_id) else {
        return Ok(None);
    };
    let base_id = wrapper_base_id(wrapper_id);
    let use_cache = !no_cache;
    let target = current_target_triples(linux_libc_override)?;
    let Some(release) = latest_release(paths, repo, use_cache, prerelease).await? else {
        return Ok(None);
    };
    resolve_python_wheel_from_release(repo, base_id, &target, &release)
        .await
        .map(Some)
}

pub async fn wrapper_python_wheel_from_release_tag(
    paths: &Paths,
    edition: &str,
    wrapper_id: &str,
    release_tag: &str,
    no_cache: bool,
    linux_libc_override: Option<LinuxLibcMode>,
) -> Result<Option<ArtifactSpec>, HackArenaError> {
    let Some(repo) = wrapper_repo_for_edition(edition, wrapper_id) else {
        return Ok(None);
    };
    let base_id = wrapper_base_id(wrapper_id);
    let use_cache = !no_cache;
    let target = current_target_triples(linux_libc_override)?;
    let Some(release) = release_by_tag(paths, repo, release_tag, use_cache).await? else {
        return Ok(None);
    };
    resolve_python_wheel_from_release(repo, base_id, &target, &release)
        .await
        .map(Some)
}

pub async fn latest_wrapper_csharp_nupkg_from_releases(
    paths: &Paths,
    edition: &str,
    wrapper_id: &str,
    no_cache: bool,
    prerelease: bool,
    linux_libc_override: Option<LinuxLibcMode>,
) -> Result<Option<ArtifactSpec>, HackArenaError> {
    let Some(repo) = wrapper_repo_for_edition(edition, wrapper_id) else {
        return Ok(None);
    };
    let base_id = wrapper_base_id(wrapper_id);
    let use_cache = !no_cache;
    let target = current_target_triples(linux_libc_override)?;
    let Some(release) = latest_release(paths, repo, use_cache, prerelease).await? else {
        return Ok(None);
    };
    resolve_csharp_nupkg_from_release(repo, base_id, &target, &release)
        .await
        .map(Some)
}

pub async fn wrapper_csharp_nupkg_from_release_tag(
    paths: &Paths,
    edition: &str,
    wrapper_id: &str,
    release_tag: &str,
    no_cache: bool,
    linux_libc_override: Option<LinuxLibcMode>,
) -> Result<Option<ArtifactSpec>, HackArenaError> {
    let Some(repo) = wrapper_repo_for_edition(edition, wrapper_id) else {
        return Ok(None);
    };
    let base_id = wrapper_base_id(wrapper_id);
    let use_cache = !no_cache;
    let target = current_target_triples(linux_libc_override)?;
    let Some(release) = release_by_tag(paths, repo, release_tag, use_cache).await? else {
        return Ok(None);
    };
    resolve_csharp_nupkg_from_release(repo, base_id, &target, &release)
        .await
        .map(Some)
}

pub async fn latest_wrapper_cpp_sdk_from_releases(
    paths: &Paths,
    edition: &str,
    wrapper_id: &str,
    no_cache: bool,
    prerelease: bool,
    linux_libc_override: Option<LinuxLibcMode>,
) -> Result<Option<ArtifactSpec>, HackArenaError> {
    let Some(repo) = wrapper_repo_for_edition(edition, wrapper_id) else {
        return Ok(None);
    };
    let base_id = wrapper_base_id(wrapper_id);
    let use_cache = !no_cache;
    let target = current_target_triples(linux_libc_override)?;
    let Some(release) = latest_release(paths, repo, use_cache, prerelease).await? else {
        return Ok(None);
    };
    resolve_cpp_sdk_from_release(repo, base_id, &target, &release)
        .await
        .map(Some)
}

pub async fn wrapper_cpp_sdk_from_release_tag(
    paths: &Paths,
    edition: &str,
    wrapper_id: &str,
    release_tag: &str,
    no_cache: bool,
    linux_libc_override: Option<LinuxLibcMode>,
) -> Result<Option<ArtifactSpec>, HackArenaError> {
    let Some(repo) = wrapper_repo_for_edition(edition, wrapper_id) else {
        return Ok(None);
    };
    let base_id = wrapper_base_id(wrapper_id);
    let use_cache = !no_cache;
    let target = current_target_triples(linux_libc_override)?;
    let Some(release) = release_by_tag(paths, repo, release_tag, use_cache).await? else {
        return Ok(None);
    };
    resolve_cpp_sdk_from_release(repo, base_id, &target, &release)
        .await
        .map(Some)
}

pub async fn latest_wrapper_typescript_tgz_from_releases(
    paths: &Paths,
    edition: &str,
    wrapper_id: &str,
    no_cache: bool,
    prerelease: bool,
    linux_libc_override: Option<LinuxLibcMode>,
) -> Result<Option<ArtifactSpec>, HackArenaError> {
    let Some(repo) = wrapper_repo_for_edition(edition, wrapper_id) else {
        return Ok(None);
    };
    let base_id = wrapper_base_id(wrapper_id);
    let use_cache = !no_cache;
    let target = current_target_triples(linux_libc_override)?;
    let Some(release) = latest_release(paths, repo, use_cache, prerelease).await? else {
        return Ok(None);
    };
    resolve_typescript_tgz_from_release(repo, base_id, &target, &release)
        .await
        .map(Some)
}

pub async fn wrapper_typescript_tgz_from_release_tag(
    paths: &Paths,
    edition: &str,
    wrapper_id: &str,
    release_tag: &str,
    no_cache: bool,
    linux_libc_override: Option<LinuxLibcMode>,
) -> Result<Option<ArtifactSpec>, HackArenaError> {
    let Some(repo) = wrapper_repo_for_edition(edition, wrapper_id) else {
        return Ok(None);
    };
    let base_id = wrapper_base_id(wrapper_id);
    let use_cache = !no_cache;
    let target = current_target_triples(linux_libc_override)?;
    let Some(release) = release_by_tag(paths, repo, release_tag, use_cache).await? else {
        return Ok(None);
    };
    resolve_typescript_tgz_from_release(repo, base_id, &target, &release)
        .await
        .map(Some)
}

fn wrapper_repo_for_edition(edition: &str, wrapper_id: &str) -> Option<&'static str> {
    let base_id = wrapper_base_id(wrapper_id);
    repos_for_edition(edition).and_then(|repos| {
        repos
            .wrappers
            .iter()
            .find(|(id, _)| id.eq_ignore_ascii_case(base_id))
            .map(|(_, repo)| *repo)
    })
}

fn repos_for_edition(edition: &str) -> Option<EditionRepos> {
    match edition {
        "3" => Some(EditionRepos {
            auth_repo: AUTH_REPO,
            backend_repo: Some(BACKEND_REPO_EDITION_3),
            wrappers: WRAPPERS_EDITION_3,
        }),
        _ => None,
    }
}

async fn resolve_required_component(
    paths: &Paths,
    repo: &str,
    component: ComponentSelector<'_>,
    target: &TargetTripleResolution,
    use_cache: bool,
    allow_prerelease: bool,
) -> Result<ResolvedReleaseAsset, HackArenaError> {
    let Some(asset) =
        resolve_optional_component(paths, repo, component, target, use_cache, allow_prerelease)
            .await?
    else {
        return Err(HackArenaError::msg(format!(
            "No GitHub release found for {} in `{repo}`.",
            component_name(component)
        )));
    };
    Ok(asset)
}

async fn resolve_optional_component(
    paths: &Paths,
    repo: &str,
    component: ComponentSelector<'_>,
    target: &TargetTripleResolution,
    use_cache: bool,
    allow_prerelease: bool,
) -> Result<Option<ResolvedReleaseAsset>, HackArenaError> {
    let Some(release) = latest_release(paths, repo, use_cache, allow_prerelease).await? else {
        return Ok(None);
    };
    resolve_component_from_release(repo, &release, component, target)
        .await
        .map(Some)
}

async fn resolve_component_from_release(
    repo: &str,
    release: &GithubRelease,
    component: ComponentSelector<'_>,
    target: &TargetTripleResolution,
) -> Result<ResolvedReleaseAsset, HackArenaError> {
    let checksums = fetch_release_checksums(repo, release).await?;
    let selected = select_component_asset(&release.assets, component, target)?;
    let expected_sha = find_checksum_for_asset(&checksums, &selected.name).ok_or_else(|| {
        HackArenaError::msg(format!(
            "Checksum for asset `{}` not found in `{CHECKSUMS_ASSET_NAME}` (repo `{repo}`, release `{}`).",
            selected.name, release.tag_name
        ))
    })?;

    Ok(ResolvedReleaseAsset {
        name: selected.name.clone(),
        url: with_asset_name_hint(&selected.url, &selected.name),
        sha256: expected_sha,
    })
}

async fn resolve_python_wheel_from_release(
    repo: &str,
    wrapper_id: &str,
    target: &TargetTripleResolution,
    release: &GithubRelease,
) -> Result<ArtifactSpec, HackArenaError> {
    let checksums = fetch_release_checksums(repo, release).await?;

    let wrapper_asset = select_component_asset(
        &release.assets,
        ComponentSelector::Wrapper(wrapper_id),
        target,
    )?;
    let wrapper_version = extract_wrapper_version_from_asset_name(&wrapper_asset.name, wrapper_id)
        .ok_or_else(|| {
            HackArenaError::msg(format!(
                "Cannot derive wrapper version from asset `{}` in `{repo}`.",
                wrapper_asset.name
            ))
        })?;
    let wheel_name = format!("hackarena3-{wrapper_version}-py3-none-any.whl");
    let wheel_asset = release
        .assets
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(&wheel_name))
        .ok_or_else(|| {
            HackArenaError::msg(format!(
                "Release `{}` in `{repo}` is missing required wheel asset `{wheel_name}`.",
                release.tag_name
            ))
        })?;
    let wheel_sha = find_checksum_for_asset(&checksums, &wheel_asset.name).ok_or_else(|| {
        HackArenaError::msg(format!(
            "Checksum for asset `{}` not found in `{CHECKSUMS_ASSET_NAME}` (repo `{repo}`, release `{}`).",
            wheel_asset.name, release.tag_name
        ))
    })?;

    Ok(ArtifactSpec {
        filename: with_asset_name_hint(&wheel_asset.url, &wheel_asset.name),
        sha256: wheel_sha,
    })
}

async fn resolve_csharp_nupkg_from_release(
    repo: &str,
    wrapper_id: &str,
    target: &TargetTripleResolution,
    release: &GithubRelease,
) -> Result<ArtifactSpec, HackArenaError> {
    let checksums = fetch_release_checksums(repo, release).await?;

    let wrapper_asset = select_component_asset(
        &release.assets,
        ComponentSelector::Wrapper(wrapper_id),
        target,
    )?;
    let wrapper_version = extract_wrapper_version_from_asset_name(&wrapper_asset.name, wrapper_id)
        .ok_or_else(|| {
            HackArenaError::msg(format!(
                "Cannot derive wrapper version from asset `{}` in `{repo}`.",
                wrapper_asset.name
            ))
        })?;
    let nupkg_name = format!("HackArena3.Wrapper.CSharp.{wrapper_version}.nupkg");
    let nupkg_asset = release
        .assets
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(&nupkg_name))
        .ok_or_else(|| {
            HackArenaError::msg(format!(
                "Release `{}` in `{repo}` is missing required runtime package `{nupkg_name}`.",
                release.tag_name
            ))
        })?;
    let nupkg_sha = find_checksum_for_asset(&checksums, &nupkg_asset.name).ok_or_else(|| {
        HackArenaError::msg(format!(
            "Checksum for asset `{}` not found in `{CHECKSUMS_ASSET_NAME}` (repo `{repo}`, release `{}`).",
            nupkg_asset.name, release.tag_name
        ))
    })?;

    Ok(ArtifactSpec {
        filename: with_asset_name_hint(&nupkg_asset.url, &nupkg_asset.name),
        sha256: nupkg_sha,
    })
}

async fn resolve_cpp_sdk_from_release(
    repo: &str,
    wrapper_id: &str,
    target: &TargetTripleResolution,
    release: &GithubRelease,
) -> Result<ArtifactSpec, HackArenaError> {
    let checksums = fetch_release_checksums(repo, release).await?;

    let wrapper_asset = select_component_asset(
        &release.assets,
        ComponentSelector::Wrapper(wrapper_id),
        target,
    )?;
    let wrapper_version = extract_wrapper_version_from_asset_name(&wrapper_asset.name, wrapper_id)
        .ok_or_else(|| {
            HackArenaError::msg(format!(
                "Cannot derive wrapper version from asset `{}` in `{repo}`.",
                wrapper_asset.name
            ))
        })?;
    resolve_cpp_sdk_asset(repo, release, &checksums, &wrapper_version, target)
}

async fn resolve_typescript_tgz_from_release(
    repo: &str,
    wrapper_id: &str,
    target: &TargetTripleResolution,
    release: &GithubRelease,
) -> Result<ArtifactSpec, HackArenaError> {
    let checksums = fetch_release_checksums(repo, release).await?;

    let wrapper_asset = select_component_asset(
        &release.assets,
        ComponentSelector::Wrapper(wrapper_id),
        target,
    )?;
    let wrapper_version = extract_wrapper_version_from_asset_name(&wrapper_asset.name, wrapper_id)
        .ok_or_else(|| {
            HackArenaError::msg(format!(
                "Cannot derive wrapper version from asset `{}` in `{repo}`.",
                wrapper_asset.name
            ))
        })?;

    resolve_typescript_runtime_tgz_asset(repo, release, &checksums, &wrapper_version)
}

fn resolve_cpp_sdk_asset(
    repo: &str,
    release: &GithubRelease,
    checksums: &HashMap<String, String>,
    wrapper_version: &str,
    target: &TargetTripleResolution,
) -> Result<ArtifactSpec, HackArenaError> {
    let candidates = cpp_sdk_asset_candidates(wrapper_version, &target.triples);
    let sdk_asset = candidates.iter().find_map(|candidate| {
        release
            .assets
            .iter()
            .find(|asset| asset.name.eq_ignore_ascii_case(candidate))
    });
    let Some(sdk_asset) = sdk_asset else {
        let available = release
            .assets
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(HackArenaError::msg(format!(
            "Release `{}` in `{repo}` is missing required C++ SDK package. Tried: {}. Expected pattern: `hackarena3-cpp-sdk-<version>-<triple>.tar.gz` (legacy Linux generic accepted: `hackarena3-cpp-sdk-<version>-Linux-<arch>.tar.gz`). Available assets: {}",
            release.tag_name,
            if candidates.is_empty() {
                "<none>".to_string()
            } else {
                candidates.join(", ")
            },
            if available.is_empty() {
                "<none>".to_string()
            } else {
                available
            }
        )));
    };
    let sdk_sha = find_checksum_for_asset(checksums, &sdk_asset.name).ok_or_else(|| {
        HackArenaError::msg(format!(
            "Checksum for asset `{}` not found in `{CHECKSUMS_ASSET_NAME}` (repo `{repo}`, release `{}`).",
            sdk_asset.name, release.tag_name
        ))
    })?;

    Ok(ArtifactSpec {
        filename: with_asset_name_hint(&sdk_asset.url, &sdk_asset.name),
        sha256: sdk_sha,
    })
}

fn resolve_typescript_runtime_tgz_asset(
    repo: &str,
    release: &GithubRelease,
    checksums: &HashMap<String, String>,
    wrapper_version: &str,
) -> Result<ArtifactSpec, HackArenaError> {
    let runtime_name = format!("hackarena3-wrapper-ts-{wrapper_version}.tgz");
    let matches = release
        .assets
        .iter()
        .filter(|asset| asset.name.eq_ignore_ascii_case(&runtime_name))
        .collect::<Vec<_>>();
    if matches.is_empty() {
        let available = release
            .assets
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(HackArenaError::msg(format!(
            "Release `{}` in `{repo}` is missing required TypeScript runtime package `{runtime_name}`. Available assets: {}",
            release.tag_name,
            if available.is_empty() {
                "<none>".to_string()
            } else {
                available
            }
        )));
    }
    if matches.len() > 1 {
        let names = matches
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(HackArenaError::msg(format!(
            "Release `{}` in `{repo}` has multiple matching TypeScript runtime packages for `{runtime_name}`: {}",
            release.tag_name, names
        )));
    }
    let runtime_asset = matches[0];

    let runtime_sha = find_checksum_for_asset(checksums, &runtime_asset.name).ok_or_else(|| {
        HackArenaError::msg(format!(
            "Checksum for asset `{}` not found in `{CHECKSUMS_ASSET_NAME}` (repo `{repo}`, release `{}`).",
            runtime_asset.name, release.tag_name
        ))
    })?;

    Ok(ArtifactSpec {
        filename: with_asset_name_hint(&runtime_asset.url, &runtime_asset.name),
        sha256: runtime_sha,
    })
}

fn cpp_sdk_asset_candidates(wrapper_version: &str, triples: &[&str]) -> Vec<String> {
    let mut out = Vec::<String>::new();
    for triple in triples {
        let sdk_triple = format!("hackarena3-cpp-sdk-{wrapper_version}-{triple}.tar.gz");
        push_case_insensitive_unique(&mut out, sdk_triple);

        let legacy = format!("HackArena3.Wrapper.Cpp.{wrapper_version}-{triple}.tar.gz");
        push_case_insensitive_unique(&mut out, legacy);

        if let Some(arch) = linux_arch_from_triple(triple) {
            let generic = format!("hackarena3-cpp-sdk-{wrapper_version}-Linux-{arch}.tar.gz");
            push_case_insensitive_unique(&mut out, generic);
        }
    }
    out
}

fn push_case_insensitive_unique(items: &mut Vec<String>, candidate: String) {
    if items
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(&candidate))
    {
        return;
    }
    items.push(candidate);
}

fn linux_arch_from_triple(triple: &str) -> Option<&str> {
    if !triple.contains("-unknown-linux-") {
        return None;
    }
    triple.split_once('-').map(|(arch, _)| arch)
}

async fn fetch_release_checksums(
    repo: &str,
    release: &GithubRelease,
) -> Result<HashMap<String, String>, HackArenaError> {
    let checksums_asset = release
        .assets
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(CHECKSUMS_ASSET_NAME))
        .ok_or_else(|| {
            HackArenaError::msg(format!(
                "Release `{}` in `{repo}` is missing `{CHECKSUMS_ASSET_NAME}`.",
                release.tag_name
            ))
        })?;
    let checksums_content = fetch_text_with_github_auth(&checksums_asset.url).await?;
    parse_sha256_sums(&checksums_content)
}

async fn latest_release(
    paths: &Paths,
    repo: &str,
    use_cache: bool,
    allow_prerelease: bool,
) -> Result<Option<GithubRelease>, HackArenaError> {
    let cache_path = cache_path_for_repo(paths, repo, allow_prerelease);
    if use_cache && cache_path.exists() && cache_is_fresh(&cache_path, RELEASE_CACHE_TTL)? {
        let bytes =
            std::fs::read(&cache_path).map_err(|e| HackArenaError::io_with_path(&cache_path, e))?;
        let release = serde_json::from_slice(&bytes)
            .map_err(|e| HackArenaError::json_with_path(&cache_path, e))?;
        return Ok(Some(release));
    }

    let url = format!("https://api.github.com/repos/{repo}/releases?per_page=20");
    let releases: Vec<GithubRelease> = fetch_json_with_github_auth(&url).await?;
    let selected = select_latest_release(&releases, allow_prerelease);

    if let Some(release) = selected.clone() {
        if let Some(parent) = cache_path.parent() {
            ensure_dir(parent)?;
        }
        let data = serde_json::to_vec_pretty(&release)
            .map_err(|e| HackArenaError::json_with_path(&cache_path, e))?;
        std::fs::write(&cache_path, data)
            .map_err(|e| HackArenaError::io_with_path(&cache_path, e))?;
    }

    Ok(selected)
}

async fn release_by_tag(
    paths: &Paths,
    repo: &str,
    release_tag: &str,
    use_cache: bool,
) -> Result<Option<GithubRelease>, HackArenaError> {
    let cache_path = cache_path_for_repo_tag(paths, repo, release_tag);
    if use_cache && cache_path.exists() && cache_is_fresh(&cache_path, RELEASE_CACHE_TTL)? {
        let bytes =
            std::fs::read(&cache_path).map_err(|e| HackArenaError::io_with_path(&cache_path, e))?;
        let release = serde_json::from_slice(&bytes)
            .map_err(|e| HackArenaError::json_with_path(&cache_path, e))?;
        return Ok(Some(release));
    }

    let Some(release) = fetch_release_by_tag(repo, release_tag).await? else {
        return Ok(None);
    };

    if let Some(parent) = cache_path.parent() {
        ensure_dir(parent)?;
    }
    let data = serde_json::to_vec_pretty(&release)
        .map_err(|e| HackArenaError::json_with_path(&cache_path, e))?;
    std::fs::write(&cache_path, data).map_err(|e| HackArenaError::io_with_path(&cache_path, e))?;

    Ok(Some(release))
}

fn cache_is_fresh(path: &Path, max_age: Duration) -> Result<bool, HackArenaError> {
    let meta = std::fs::metadata(path).map_err(|e| HackArenaError::io_with_path(path, e))?;
    let modified = match meta.modified() {
        Ok(ts) => ts,
        Err(_) => return Ok(false),
    };
    let now = SystemTime::now();
    let age = match now.duration_since(modified) {
        Ok(age) => age,
        Err(_) => return Ok(false),
    };
    Ok(age <= max_age)
}

fn select_latest_release(
    releases: &[GithubRelease],
    allow_prerelease: bool,
) -> Option<GithubRelease> {
    if let Some(stable) = releases.iter().find(|r| !r.draft && !r.prerelease) {
        return Some(stable.clone());
    }
    if allow_prerelease {
        return releases.iter().find(|r| !r.draft).cloned();
    }
    None
}

fn cache_path_for_repo(paths: &Paths, repo: &str, allow_prerelease: bool) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(repo.as_bytes());
    let mode_marker: &[u8] = if allow_prerelease {
        b":allow_prerelease"
    } else {
        b":stable_only"
    };
    hasher.update(mode_marker);
    let key = hex::encode(hasher.finalize());
    paths
        .releases_cache_dir()
        .join(key)
        .join("latest_release.json")
}

fn cache_path_for_repo_tag(paths: &Paths, repo: &str, release_tag: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(repo.as_bytes());
    hasher.update(b":");
    hasher.update(release_tag.as_bytes());
    let key = hex::encode(hasher.finalize());
    paths
        .releases_cache_dir()
        .join(key)
        .join("release_by_tag.json")
}

fn select_component_asset<'a>(
    assets: &'a [GithubAsset],
    component: ComponentSelector<'_>,
    target: &TargetTripleResolution,
) -> Result<&'a GithubAsset, HackArenaError> {
    match component {
        ComponentSelector::Wrapper(wrapper_id) => {
            select_wrapper_asset_for_targets(assets, wrapper_id, target)
        }
        _ => select_single_component_asset_for_targets(assets, component, target),
    }
}

fn select_single_component_asset_for_targets<'a>(
    assets: &'a [GithubAsset],
    component: ComponentSelector<'_>,
    target: &TargetTripleResolution,
) -> Result<&'a GithubAsset, HackArenaError> {
    for triple in &target.triples {
        let matches = assets
            .iter()
            .filter(|a| is_component_asset(&a.name, component, triple))
            .collect::<Vec<_>>();

        if matches.is_empty() {
            continue;
        }
        if matches.len() > 1 {
            let names = matches
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(HackArenaError::msg(format!(
                "Multiple assets matched {} for platform `{triple}`: {}",
                component_name(component),
                names
            )));
        }
        return Ok(matches[0]);
    }
    Err(no_asset_error_for_targets(assets, component, target))
}

fn select_wrapper_asset_for_targets<'a>(
    assets: &'a [GithubAsset],
    wrapper_id: &str,
    target: &TargetTripleResolution,
) -> Result<&'a GithubAsset, HackArenaError> {
    let component = ComponentSelector::Wrapper(wrapper_id);
    for triple in &target.triples {
        let platform_matches = assets
            .iter()
            .filter(|a| is_wrapper_platform_asset(&a.name, wrapper_id, triple))
            .collect::<Vec<_>>();
        if platform_matches.len() > 1 {
            let names = platform_matches
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(HackArenaError::msg(format!(
                "Multiple platform-specific assets matched {} for platform `{triple}`: {}",
                component_name(component),
                names
            )));
        }
        if let Some(selected) = platform_matches.first() {
            return Ok(selected);
        }
    }

    if wrapper_id.eq_ignore_ascii_case("typescript") {
        let standard_universal_matches = assets
            .iter()
            .filter(|a| is_wrapper_standard_universal_asset(&a.name, wrapper_id))
            .collect::<Vec<_>>();
        if standard_universal_matches.len() > 1 {
            let names = standard_universal_matches
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(HackArenaError::msg(format!(
                "Multiple universal assets matched {}: {}",
                component_name(component),
                names
            )));
        }
        if let Some(selected) = standard_universal_matches.first() {
            return Ok(selected);
        }

        let custom_universal_matches = assets
            .iter()
            .filter(|a| is_typescript_custom_universal_wrapper_asset(&a.name, wrapper_id))
            .collect::<Vec<_>>();
        if custom_universal_matches.len() > 1 {
            let names = custom_universal_matches
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(HackArenaError::msg(format!(
                "Multiple universal assets matched {}: {}",
                component_name(component),
                names
            )));
        }
        if let Some(selected) = custom_universal_matches.first() {
            return Ok(selected);
        }

        return Err(no_asset_error_for_targets(assets, component, target));
    }

    let universal_matches = assets
        .iter()
        .filter(|a| is_wrapper_universal_asset(&a.name, wrapper_id))
        .collect::<Vec<_>>();
    if universal_matches.len() > 1 {
        let names = universal_matches
            .iter()
            .map(|a| a.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(HackArenaError::msg(format!(
            "Multiple universal assets matched {}: {}",
            component_name(component),
            names
        )));
    }
    if let Some(selected) = universal_matches.first() {
        return Ok(selected);
    }

    Err(no_asset_error_for_targets(assets, component, target))
}

fn no_asset_error_for_targets(
    assets: &[GithubAsset],
    component: ComponentSelector<'_>,
    target: &TargetTripleResolution,
) -> HackArenaError {
    let available = assets
        .iter()
        .map(|a| a.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let tried = target.triples.join(", ");
    let linux_details = target
        .linux_details
        .as_ref()
        .map(|details| {
            format!(
                " Linux libc mode: `{}` (source: {}, order: {}).",
                details.mode.as_str(),
                details.source,
                details.order_label
            )
        })
        .unwrap_or_default();
    HackArenaError::msg(format!(
        "No asset matching {} for tried platform(s) `{tried}`.{} Expected pattern: {}. Available assets: {}",
        component_name(component),
        linux_details,
        expected_pattern_for_targets(component, &target.triples),
        if available.is_empty() {
            "<none>".to_string()
        } else {
            available
        }
    ))
}

fn expected_pattern_for_targets(component: ComponentSelector<'_>, triples: &[&str]) -> String {
    let mut patterns = Vec::<String>::new();
    for triple in triples {
        let pattern = expected_pattern(component, triple);
        if !patterns.contains(&pattern) {
            patterns.push(pattern);
        }
    }
    if patterns.is_empty() {
        return expected_pattern(component, "unknown");
    }
    patterns.join(" OR ")
}

fn is_component_asset(name: &str, component: ComponentSelector<'_>, triple: &str) -> bool {
    let lower = name.to_ascii_lowercase();

    match component {
        ComponentSelector::Auth => {
            let triple_part = format!("-{}", triple.to_ascii_lowercase());
            lower.contains(&triple_part)
                && lower.starts_with("ha-auth-v")
                && is_auth_extension(&lower)
        }
        ComponentSelector::Backend => is_backend_local_asset(&lower, triple),
        ComponentSelector::Wrapper(wrapper_id) => {
            is_wrapper_platform_asset(&lower, wrapper_id, triple)
                || is_wrapper_universal_asset(&lower, wrapper_id)
        }
    }
}

fn expected_pattern(component: ComponentSelector<'_>, triple: &str) -> String {
    match component {
        ComponentSelector::Auth => format!("ha-auth-v<version>-{triple}.<exe|zip|tar.gz>"),
        ComponentSelector::Backend => {
            format!("*-backend-local-{triple}-v<version>.<zip|tar.gz>")
        }
        ComponentSelector::Wrapper(wrapper_id) => {
            if wrapper_id.eq_ignore_ascii_case("typescript") {
                return format!(
                    "wrapper-typescript-v<version>-{triple}.<zip|tar.gz> OR wrapper-typescript-v<version>.<zip|tar.gz> OR hackarena3-template-ts-v<version>.<zip|tar.gz>"
                );
            }
            format!(
                "wrapper-{wrapper_id}-v<version>-{triple}.<zip|tar.gz> OR wrapper-{wrapper_id}-v<version>.<zip|tar.gz>"
            )
        }
    }
}

fn component_name(component: ComponentSelector<'_>) -> String {
    match component {
        ComponentSelector::Auth => "auth".to_string(),
        ComponentSelector::Backend => "backend".to_string(),
        ComponentSelector::Wrapper(wrapper_id) => format!("wrapper `{wrapper_id}`"),
    }
}

fn is_archive_extension(name_lower: &str) -> bool {
    name_lower.ends_with(".zip") || name_lower.ends_with(".tar.gz")
}

fn is_auth_extension(name_lower: &str) -> bool {
    name_lower.ends_with(".exe") || is_archive_extension(name_lower)
}

fn is_backend_local_asset(name_lower: &str, triple: &str) -> bool {
    let triple_part = format!("-{}", triple.to_ascii_lowercase());
    name_lower.contains("-backend-local-")
        && name_lower.contains(&triple_part)
        && name_lower.contains("-v")
        && is_archive_extension(name_lower)
}

fn is_wrapper_platform_asset(name: &str, wrapper_id: &str, triple: &str) -> bool {
    let name_lower = name.to_ascii_lowercase();
    let triple_part = format!("-{}", triple.to_ascii_lowercase());
    let prefix = format!("wrapper-{}-v", wrapper_id.to_ascii_lowercase());
    name_lower.starts_with(&prefix)
        && name_lower.contains(&triple_part)
        && is_archive_extension(&name_lower)
}

fn is_wrapper_universal_asset(name: &str, wrapper_id: &str) -> bool {
    is_wrapper_standard_universal_asset(name, wrapper_id)
        || is_typescript_custom_universal_wrapper_asset(name, wrapper_id)
}

fn is_wrapper_standard_universal_asset(name: &str, wrapper_id: &str) -> bool {
    let name_lower = name.to_ascii_lowercase();
    let prefix = format!("wrapper-{}-v", wrapper_id.to_ascii_lowercase());
    if !name_lower.starts_with(&prefix) || !is_archive_extension(&name_lower) {
        return false;
    }
    let Some(stem) = strip_archive_extension(&name_lower) else {
        return false;
    };
    if !stem.starts_with(&prefix) {
        return false;
    }
    if known_target_triples()
        .iter()
        .any(|triple| stem.contains(&format!("-{triple}")))
    {
        return false;
    }
    stem.len() > prefix.len()
}

fn strip_archive_extension(name_lower: &str) -> Option<&str> {
    if let Some(stem) = name_lower.strip_suffix(".tar.gz") {
        return Some(stem);
    }
    name_lower.strip_suffix(".zip")
}

fn extract_wrapper_version_from_asset_name(asset_name: &str, wrapper_id: &str) -> Option<String> {
    let stem = strip_archive_extension(asset_name)?;
    let prefix = format!("wrapper-{}-v", wrapper_id.to_ascii_lowercase());
    let stem_lower = stem.to_ascii_lowercase();
    if stem_lower.starts_with(&prefix) {
        let mut version = stem.get(prefix.len()..)?.to_string();
        if version.is_empty() {
            return None;
        }
        for triple in known_target_triples() {
            let suffix = format!("-{triple}");
            if version.to_ascii_lowercase().ends_with(&suffix) {
                let new_len = version.len().saturating_sub(suffix.len());
                version.truncate(new_len);
                break;
            }
        }
        if version.is_empty() {
            return None;
        }
        return Some(version);
    }

    if wrapper_id.eq_ignore_ascii_case("typescript") {
        return extract_typescript_custom_template_version(stem);
    }
    None
}

fn is_typescript_custom_universal_wrapper_asset(name: &str, wrapper_id: &str) -> bool {
    if !wrapper_id.eq_ignore_ascii_case("typescript") {
        return false;
    }
    let name_lower = name.to_ascii_lowercase();
    if !is_archive_extension(&name_lower) {
        return false;
    }
    let Some(stem) = strip_archive_extension(&name_lower) else {
        return false;
    };
    extract_typescript_custom_template_version(stem).is_some()
}

fn extract_typescript_custom_template_version(stem: &str) -> Option<String> {
    let stem_lower = stem.to_ascii_lowercase();
    let prefix = "hackarena3-template-ts-v";
    if !stem_lower.starts_with(prefix) {
        return None;
    }
    let version = stem.get(prefix.len()..)?.to_string();
    if version.is_empty() {
        return None;
    }
    Some(version)
}

fn known_target_triples() -> &'static [&'static str] {
    &[
        "x86_64-pc-windows-msvc",
        "aarch64-pc-windows-msvc",
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "x86_64-unknown-linux-musl",
        "aarch64-unknown-linux-musl",
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
    ]
}

pub fn linux_libc_verbose_summary(
    linux_libc_override: Option<LinuxLibcMode>,
) -> Result<Option<String>, HackArenaError> {
    if !cfg!(target_os = "linux") {
        return Ok(None);
    }
    let target = current_target_triples(linux_libc_override)?;
    let Some(details) = target.linux_details else {
        return Ok(None);
    };
    Ok(Some(format!(
        "{} (source: {}, order: {})",
        details.mode.as_str(),
        details.source,
        details.order_label
    )))
}

fn current_target_triples(
    linux_libc_override: Option<LinuxLibcMode>,
) -> Result<TargetTripleResolution, HackArenaError> {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        return Ok(TargetTripleResolution {
            triples: vec!["x86_64-pc-windows-msvc"],
            linux_details: None,
        });
    }
    if cfg!(all(target_os = "windows", target_arch = "aarch64")) {
        return Ok(TargetTripleResolution {
            triples: vec!["aarch64-pc-windows-msvc"],
            linux_details: None,
        });
    }
    if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        return Ok(TargetTripleResolution {
            triples: vec!["x86_64-apple-darwin"],
            linux_details: None,
        });
    }
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        return Ok(TargetTripleResolution {
            triples: vec!["aarch64-apple-darwin"],
            linux_details: None,
        });
    }
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        let (triples, details) =
            linux_target_triples_for_arch("x86_64", resolve_linux_mode(linux_libc_override)?)?;
        return Ok(TargetTripleResolution {
            triples,
            linux_details: Some(details),
        });
    }
    if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        let (triples, details) =
            linux_target_triples_for_arch("aarch64", resolve_linux_mode(linux_libc_override)?)?;
        return Ok(TargetTripleResolution {
            triples,
            linux_details: Some(details),
        });
    }

    Err(HackArenaError::msg(
        "This platform is not supported by GitHub release artifact mapping.",
    ))
}

fn linux_target_triples_for_arch(
    arch: &str,
    details: LinuxModeDetails,
) -> Result<(Vec<&'static str>, LinuxModeDetails), HackArenaError> {
    let (gnu, musl) = match arch {
        "x86_64" => ("x86_64-unknown-linux-gnu", "x86_64-unknown-linux-musl"),
        "aarch64" => ("aarch64-unknown-linux-gnu", "aarch64-unknown-linux-musl"),
        _ => {
            return Err(HackArenaError::msg(format!(
                "Linux architecture `{arch}` is not supported by release artifact mapping."
            )));
        }
    };

    let triples = match details.order_label {
        "gnu->musl" => vec![gnu, musl],
        "musl->gnu" => vec![musl, gnu],
        "gnu" => vec![gnu],
        "musl" => vec![musl],
        _ => vec![musl, gnu],
    };
    Ok((triples, details))
}

fn resolve_linux_mode(
    linux_libc_override: Option<LinuxLibcMode>,
) -> Result<LinuxModeDetails, HackArenaError> {
    let env_value = linux_libc_env_value()?;
    let auto_order = auto_linux_libc_order_label();
    resolve_linux_mode_from_inputs(linux_libc_override, env_value.as_deref(), auto_order)
}

fn resolve_linux_mode_from_inputs(
    linux_libc_override: Option<LinuxLibcMode>,
    env_value: Option<&str>,
    auto_order_label: &'static str,
) -> Result<LinuxModeDetails, HackArenaError> {
    if let Some(mode) = linux_libc_override {
        let order_label = match mode {
            LinuxLibcMode::Auto => auto_order_label,
            LinuxLibcMode::Gnu => "gnu",
            LinuxLibcMode::Musl => "musl",
        };
        return Ok(LinuxModeDetails {
            mode,
            source: "flag",
            order_label,
        });
    }

    if let Some(raw) = env_value {
        let mode = LinuxLibcMode::parse(raw).ok_or_else(|| {
            HackArenaError::msg(format!(
                "Invalid {LINUX_LIBC_ENV} value `{raw}`. Expected one of: auto, gnu, musl."
            ))
        })?;
        let order_label = match mode {
            LinuxLibcMode::Auto => auto_order_label,
            LinuxLibcMode::Gnu => "gnu",
            LinuxLibcMode::Musl => "musl",
        };
        return Ok(LinuxModeDetails {
            mode,
            source: "env",
            order_label,
        });
    }

    Ok(LinuxModeDetails {
        mode: LinuxLibcMode::Auto,
        source: "default",
        order_label: auto_order_label,
    })
}

fn linux_libc_env_value() -> Result<Option<String>, HackArenaError> {
    let Ok(value) = std::env::var(LINUX_LIBC_ENV) else {
        return Ok(None);
    };
    let trimmed = value.trim().to_string();
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(Some(trimmed))
}

fn auto_linux_libc_order_label() -> &'static str {
    if os_release_id_is_nixos().unwrap_or(false) {
        return "musl->gnu";
    }
    if let Some(ldd) = read_ldd_version_output() {
        let lower = ldd.to_ascii_lowercase();
        if lower.contains("musl") {
            return "musl->gnu";
        }
        if lower.contains("glibc") || lower.contains("gnu libc") {
            return "gnu->musl";
        }
    }
    "musl->gnu"
}

fn os_release_id_is_nixos() -> Result<bool, HackArenaError> {
    let path = Path::new("/etc/os-release");
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(HackArenaError::io_with_path(path, e)),
    };
    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("ID=") {
            continue;
        }
        let value = trimmed
            .trim_start_matches("ID=")
            .trim_matches('"')
            .trim_matches('\'')
            .trim();
        return Ok(value.eq_ignore_ascii_case("nixos"));
    }
    Ok(false)
}

fn read_ldd_version_output() -> Option<String> {
    let output = Command::new("ldd").arg("--version").output().ok()?;
    if output.stdout.is_empty() && output.stderr.is_empty() {
        return None;
    }
    let mut text = String::new();
    if !output.stdout.is_empty() {
        text.push_str(&String::from_utf8_lossy(&output.stdout));
    }
    if !output.stderr.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    Some(text)
}

fn find_checksum_for_asset(
    checksums: &HashMap<String, String>,
    asset_name: &str,
) -> Option<String> {
    if let Some(value) = checksums.get(asset_name) {
        return Some(value.clone());
    }
    checksums
        .iter()
        .find(|(name, _)| name.ends_with(asset_name))
        .map(|(_, sha)| sha.clone())
}

pub fn parse_sha256_sums(content: &str) -> Result<HashMap<String, String>, HackArenaError> {
    let mut out = HashMap::new();

    for (idx, raw_line) in content.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        let mut parts = line.splitn(2, char::is_whitespace);
        let Some(sha) = parts.next() else {
            continue;
        };
        let Some(rest) = parts.next() else {
            continue;
        };

        let sha = sha.trim().to_ascii_lowercase();
        if sha.len() != 64 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(HackArenaError::msg(format!(
                "Invalid SHA256 in {CHECKSUMS_ASSET_NAME} at line {}.",
                idx + 1
            )));
        }

        let filename = rest.trim_start().trim_start_matches('*').trim();
        if filename.is_empty() {
            return Err(HackArenaError::msg(format!(
                "Missing filename in {CHECKSUMS_ASSET_NAME} at line {}.",
                idx + 1
            )));
        }
        out.insert(filename.to_string(), sha);
    }

    if out.is_empty() {
        return Err(HackArenaError::msg(format!(
            "{CHECKSUMS_ASSET_NAME} did not contain any entries."
        )));
    }

    Ok(out)
}

fn infer_bin_name_auth(from_artifact_name: &str) -> String {
    let lower = from_artifact_name.to_ascii_lowercase();
    if lower.starts_with("ha-auth") {
        return default_bin_name_auth();
    }
    if lower.ends_with(".zip") || lower.ends_with(".tar.gz") {
        return default_bin_name_auth();
    }
    from_artifact_name.to_string()
}

fn default_bin_name_auth() -> String {
    if cfg!(windows) {
        "ha-auth.exe".to_string()
    } else {
        "ha-auth".to_string()
    }
}

async fn fetch_json_with_github_auth<T: DeserializeOwned>(url: &str) -> Result<T, HackArenaError> {
    let client = reqwest::Client::new();
    let token = github_token();
    let resp = get_with_token_fallback(&client, url, token.as_deref()).await?;
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| HackArenaError::http_with_url(url, e))?;
    serde_json::from_slice(&bytes).map_err(|e| HackArenaError::json_with_url(url, e))
}

async fn fetch_text_with_github_auth(url: &str) -> Result<String, HackArenaError> {
    let client = reqwest::Client::new();
    let token = github_token();
    let resp = get_with_token_fallback(&client, url, token.as_deref()).await?;
    resp.text()
        .await
        .map_err(|e| HackArenaError::http_with_url(url, e))
}

async fn fetch_release_by_tag(
    repo: &str,
    release_tag: &str,
) -> Result<Option<GithubRelease>, HackArenaError> {
    let client = reqwest::Client::new();
    let token = github_token();
    let url = format!("https://api.github.com/repos/{repo}/releases/tags/{release_tag}");

    let resp = get_with_token_fallback_allow_404(&client, &url, token.as_deref()).await?;
    let Some(resp) = resp else {
        return Ok(None);
    };
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| HackArenaError::http_with_url(&url, e))?;
    let parsed =
        serde_json::from_slice(&bytes).map_err(|e| HackArenaError::json_with_url(&url, e))?;
    Ok(Some(parsed))
}

fn github_token() -> Option<String> {
    for key in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(value) = std::env::var(key) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

async fn get_with_token_fallback(
    client: &reqwest::Client,
    url: &str,
    token: Option<&str>,
) -> Result<reqwest::Response, HackArenaError> {
    let token_present = token.is_some();
    let resp = send_get(client, url, token).await?;
    if matches!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
    ) && token_present
    {
        let anon = send_get(client, url, None).await?;
        if anon.status().is_success() {
            return Ok(anon);
        }
        return Err(github_http_status_error(url, anon.status(), false));
    }

    if resp.status().is_success() {
        return Ok(resp);
    }
    Err(github_http_status_error(url, resp.status(), token_present))
}

async fn get_with_token_fallback_allow_404(
    client: &reqwest::Client,
    url: &str,
    token: Option<&str>,
) -> Result<Option<reqwest::Response>, HackArenaError> {
    let token_present = token.is_some();
    let resp = send_get(client, url, token).await?;
    if matches!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
    ) && token_present
    {
        let anon = send_get(client, url, None).await?;
        if anon.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if anon.status().is_success() {
            return Ok(Some(anon));
        }
        return Err(github_http_status_error(url, anon.status(), false));
    }

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if resp.status().is_success() {
        return Ok(Some(resp));
    }
    Err(github_http_status_error(url, resp.status(), token_present))
}

async fn send_get(
    client: &reqwest::Client,
    url: &str,
    token: Option<&str>,
) -> Result<reqwest::Response, HackArenaError> {
    let accept = if is_release_asset_api_url(url) {
        "application/octet-stream"
    } else {
        "application/vnd.github+json"
    };
    let mut req = client
        .get(url)
        .header(reqwest::header::USER_AGENT, "hackarena-cli")
        .header(reqwest::header::ACCEPT, accept);
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }
    req.send()
        .await
        .map_err(|e| HackArenaError::http_with_url(url, e))
}

fn is_release_asset_api_url(url: &str) -> bool {
    url.contains("api.github.com/repos/") && url.contains("/releases/assets/")
}

fn with_asset_name_hint(url: &str, asset_name: &str) -> String {
    let sep = if url.contains('?') { '&' } else { '?' };
    format!("{url}{sep}asset_name={asset_name}")
}

fn github_http_status_error(
    url: &str,
    status: reqwest::StatusCode,
    token_present: bool,
) -> HackArenaError {
    if status == reqwest::StatusCode::NOT_FOUND && url.contains("api.github.com/repos/") {
        if token_present {
            return HackArenaError::msg(format!(
                "GitHub API returned 404 for `{url}`. The repository may be private and GH_TOKEN/GITHUB_TOKEN does not have access, or the owner/repo is incorrect."
            ));
        }
        return HackArenaError::msg(format!(
            "GitHub API returned 404 for `{url}`. For private repositories this is expected without authentication. Set GH_TOKEN (or GITHUB_TOKEN) with repo read access."
        ));
    }
    HackArenaError::msg(format!("HTTP {} for `{url}`", status.as_u16()))
}

#[cfg(test)]
mod tests {
    use super::{
        ComponentSelector, GithubAsset, GithubRelease, LinuxLibcMode, TargetTripleResolution,
        cpp_sdk_asset_candidates, extract_wrapper_version_from_asset_name, is_component_asset,
        linux_target_triples_for_arch, parse_sha256_sums, resolve_cpp_sdk_asset,
        resolve_linux_mode_from_inputs, resolve_typescript_runtime_tgz_asset,
        select_component_asset, select_latest_release, wrapper_base_id,
    };
    use std::collections::HashMap;

    fn asset(name: &str) -> GithubAsset {
        GithubAsset {
            name: name.to_string(),
            url: format!("https://example.test/{name}"),
            browser_download_url: format!("https://example.test/{name}"),
        }
    }

    fn target(triples: &[&'static str]) -> TargetTripleResolution {
        TargetTripleResolution {
            triples: triples.to_vec(),
            linux_details: None,
        }
    }

    #[test]
    fn parse_sha256_sums_accepts_common_formats() {
        let content = "\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  ha-auth-v0.2.0-x86_64-pc-windows-msvc.exe\r\n\
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb *ha3-backend-local-x86_64-pc-windows-msvc-v0.1.0-beta.1.zip\n\
";
        let parsed = parse_sha256_sums(content).expect("checksum parse should pass");
        assert_eq!(
            parsed
                .get("ha-auth-v0.2.0-x86_64-pc-windows-msvc.exe")
                .expect("auth entry"),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(
            parsed
                .get("ha3-backend-local-x86_64-pc-windows-msvc-v0.1.0-beta.1.zip")
                .expect("backend entry"),
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );
    }

    #[test]
    fn auth_asset_pattern_matches_known_names() {
        assert!(is_component_asset(
            "ha-auth-v0.2.0-x86_64-pc-windows-msvc.exe",
            ComponentSelector::Auth,
            "x86_64-pc-windows-msvc"
        ));
        assert!(is_component_asset(
            "ha-auth-v0.2.0-aarch64-unknown-linux-musl.tar.gz",
            ComponentSelector::Auth,
            "aarch64-unknown-linux-musl"
        ));
        assert!(!is_component_asset(
            "ha-auth-v0.2.0-x86_64-pc-windows-msvc.exe",
            ComponentSelector::Auth,
            "aarch64-pc-windows-msvc"
        ));
    }

    #[test]
    fn backend_and_wrapper_patterns_are_enforced() {
        assert!(is_component_asset(
            "ha3-backend-local-x86_64-pc-windows-msvc-v0.1.0-beta.1.zip",
            ComponentSelector::Backend,
            "x86_64-pc-windows-msvc"
        ));
        assert!(!is_component_asset(
            "ha3-backend-official-x86_64-pc-windows-msvc-v0.1.0-beta.1.zip",
            ComponentSelector::Backend,
            "x86_64-pc-windows-msvc"
        ));
        assert!(is_component_asset(
            "wrapper-python-v0.1.0-x86_64-unknown-linux-musl.zip",
            ComponentSelector::Wrapper("python"),
            "x86_64-unknown-linux-musl"
        ));
        assert!(is_component_asset(
            "ha3-backend-local-x86_64-unknown-linux-gnu-v0.1.0-beta.1.zip",
            ComponentSelector::Backend,
            "x86_64-unknown-linux-gnu"
        ));
        assert!(is_component_asset(
            "wrapper-python-v0.1.0b1.zip",
            ComponentSelector::Wrapper("python"),
            "x86_64-unknown-linux-musl"
        ));
        assert!(is_component_asset(
            "wrapper-csharp-v0.1.0-beta.1.zip",
            ComponentSelector::Wrapper("csharp"),
            "x86_64-pc-windows-msvc"
        ));
        assert!(is_component_asset(
            "wrapper-cpp-v0.1.0b8.zip",
            ComponentSelector::Wrapper("cpp"),
            "x86_64-pc-windows-msvc"
        ));
        assert!(is_component_asset(
            "wrapper-typescript-v0.2.0-beta.1.zip",
            ComponentSelector::Wrapper("typescript"),
            "x86_64-pc-windows-msvc"
        ));
        assert!(is_component_asset(
            "hackarena3-template-ts-v0.2.0-beta.1.zip",
            ComponentSelector::Wrapper("typescript"),
            "x86_64-pc-windows-msvc"
        ));
        assert!(!is_component_asset(
            "python-wrapper-v0.1.0-x86_64-unknown-linux-musl.zip",
            ComponentSelector::Wrapper("python"),
            "x86_64-unknown-linux-musl"
        ));
    }

    #[test]
    fn wrapper_asset_selection_prefers_platform_then_universal() {
        let assets = vec![
            asset("wrapper-python-v0.1.0b1.zip"),
            asset("wrapper-python-v0.1.0b1-x86_64-unknown-linux-musl.zip"),
        ];

        let selected = select_component_asset(
            &assets,
            ComponentSelector::Wrapper("python"),
            &target(&["x86_64-unknown-linux-musl"]),
        )
        .expect("select should pass");
        assert_eq!(
            selected.name,
            "wrapper-python-v0.1.0b1-x86_64-unknown-linux-musl.zip"
        );
    }

    #[test]
    fn wrapper_asset_selection_falls_back_to_universal() {
        let assets = vec![asset("wrapper-python-v0.1.0b1.zip")];

        let selected = select_component_asset(
            &assets,
            ComponentSelector::Wrapper("python"),
            &target(&["x86_64-pc-windows-msvc"]),
        )
        .expect("select should pass");
        assert_eq!(selected.name, "wrapper-python-v0.1.0b1.zip");
    }

    #[test]
    fn typescript_wrapper_asset_selection_accepts_custom_universal_template() {
        let assets = vec![asset("hackarena3-template-ts-v0.2.0-beta.1.zip")];

        let selected = select_component_asset(
            &assets,
            ComponentSelector::Wrapper("typescript"),
            &target(&["x86_64-pc-windows-msvc"]),
        )
        .expect("select should pass");
        assert_eq!(selected.name, "hackarena3-template-ts-v0.2.0-beta.1.zip");
    }

    #[test]
    fn typescript_wrapper_asset_selection_prefers_standard_universal_over_custom() {
        let assets = vec![
            asset("hackarena3-template-ts-v0.2.0-beta.1.zip"),
            asset("wrapper-typescript-v0.2.0-beta.1.zip"),
        ];

        let selected = select_component_asset(
            &assets,
            ComponentSelector::Wrapper("typescript"),
            &target(&["x86_64-pc-windows-msvc"]),
        )
        .expect("select should pass");
        assert_eq!(selected.name, "wrapper-typescript-v0.2.0-beta.1.zip");
    }

    #[test]
    fn extract_wrapper_version_supports_typescript_custom_template() {
        let version = extract_wrapper_version_from_asset_name(
            "hackarena3-template-ts-v0.2.0-beta.1.zip",
            "typescript",
        )
        .expect("version");
        assert_eq!(version, "0.2.0-beta.1");
    }

    #[test]
    fn resolve_typescript_runtime_tgz_asset_requires_exact_runtime_and_checksum() {
        let release = GithubRelease {
            tag_name: "v0.2.0-beta.1".to_string(),
            name: "v0.2.0-beta.1".to_string(),
            draft: false,
            prerelease: true,
            assets: vec![asset("hackarena3-wrapper-ts-0.2.0-beta.1.tgz")],
        };
        let checksums = HashMap::from([(
            "hackarena3-wrapper-ts-0.2.0-beta.1.tgz".to_string(),
            "abc".to_string(),
        )]);

        let resolved =
            resolve_typescript_runtime_tgz_asset("org/repo", &release, &checksums, "0.2.0-beta.1")
                .expect("runtime should resolve");
        assert!(
            resolved
                .filename
                .contains("hackarena3-wrapper-ts-0.2.0-beta.1.tgz")
        );
        assert_eq!(resolved.sha256, "abc");
    }

    #[test]
    fn resolve_typescript_runtime_tgz_asset_fails_without_checksum() {
        let release = GithubRelease {
            tag_name: "v0.2.0-beta.1".to_string(),
            name: "v0.2.0-beta.1".to_string(),
            draft: false,
            prerelease: true,
            assets: vec![asset("hackarena3-wrapper-ts-0.2.0-beta.1.tgz")],
        };
        let checksums = HashMap::new();

        let err =
            resolve_typescript_runtime_tgz_asset("org/repo", &release, &checksums, "0.2.0-beta.1")
                .expect_err("missing checksum should fail");
        assert!(err.to_string().contains("Checksum for asset"));
    }

    #[test]
    fn resolve_typescript_runtime_tgz_asset_fails_for_multiple_matches() {
        let release = GithubRelease {
            tag_name: "v0.2.0-beta.1".to_string(),
            name: "v0.2.0-beta.1".to_string(),
            draft: false,
            prerelease: true,
            assets: vec![
                asset("hackarena3-wrapper-ts-0.2.0-beta.1.tgz"),
                asset("HACKARENA3-WRAPPER-TS-0.2.0-BETA.1.TGZ"),
            ],
        };
        let checksums = HashMap::new();

        let err =
            resolve_typescript_runtime_tgz_asset("org/repo", &release, &checksums, "0.2.0-beta.1")
                .expect_err("multiple matches should fail");
        assert!(
            err.to_string()
                .contains("multiple matching TypeScript runtime")
        );
    }

    #[test]
    fn wrapper_asset_selection_rejects_multiple_universal_matches() {
        let assets = vec![
            asset("wrapper-python-v0.1.0b1.zip"),
            asset("wrapper-python-v0.1.0b2.zip"),
        ];

        let err = select_component_asset(
            &assets,
            ComponentSelector::Wrapper("python"),
            &target(&["x86_64-pc-windows-msvc"]),
        )
        .expect_err("expected conflict");
        assert!(
            err.to_string()
                .contains("Multiple universal assets matched")
        );
    }

    #[test]
    fn cpp_sdk_candidates_follow_target_order_and_include_linux_generic() {
        let candidates = cpp_sdk_asset_candidates(
            "0.1.0b8",
            &["x86_64-unknown-linux-gnu", "x86_64-unknown-linux-musl"],
        );
        assert_eq!(
            candidates,
            vec![
                "hackarena3-cpp-sdk-0.1.0b8-x86_64-unknown-linux-gnu.tar.gz",
                "HackArena3.Wrapper.Cpp.0.1.0b8-x86_64-unknown-linux-gnu.tar.gz",
                "hackarena3-cpp-sdk-0.1.0b8-Linux-x86_64.tar.gz",
                "hackarena3-cpp-sdk-0.1.0b8-x86_64-unknown-linux-musl.tar.gz",
                "HackArena3.Wrapper.Cpp.0.1.0b8-x86_64-unknown-linux-musl.tar.gz",
            ]
        );
    }

    #[test]
    fn cpp_sdk_resolver_prefers_first_matching_candidate() {
        let release = GithubRelease {
            tag_name: "v0.1.0b8".to_string(),
            name: "v0.1.0b8".to_string(),
            draft: false,
            prerelease: true,
            assets: vec![
                asset("hackarena3-cpp-sdk-0.1.0b8-Linux-x86_64.tar.gz"),
                asset("hackarena3-cpp-sdk-0.1.0b8-x86_64-unknown-linux-gnu.tar.gz"),
            ],
        };
        let checksums = HashMap::from([
            (
                "hackarena3-cpp-sdk-0.1.0b8-Linux-x86_64.tar.gz".to_string(),
                "legacy".to_string(),
            ),
            (
                "hackarena3-cpp-sdk-0.1.0b8-x86_64-unknown-linux-gnu.tar.gz".to_string(),
                "gnu".to_string(),
            ),
        ]);
        let selected = resolve_cpp_sdk_asset(
            "org/repo",
            &release,
            &checksums,
            "0.1.0b8",
            &target(&["x86_64-unknown-linux-gnu", "x86_64-unknown-linux-musl"]),
        )
        .expect("asset should resolve");
        assert!(selected.filename.contains("x86_64-unknown-linux-gnu"));
        assert_eq!(selected.sha256, "gnu");
    }

    #[test]
    fn cpp_sdk_resolver_falls_back_to_legacy_linux_generic() {
        let release = GithubRelease {
            tag_name: "v0.1.0b8".to_string(),
            name: "v0.1.0b8".to_string(),
            draft: false,
            prerelease: true,
            assets: vec![asset("hackarena3-cpp-sdk-0.1.0b8-Linux-x86_64.tar.gz")],
        };
        let checksums = HashMap::from([(
            "hackarena3-cpp-sdk-0.1.0b8-Linux-x86_64.tar.gz".to_string(),
            "legacy".to_string(),
        )]);
        let selected = resolve_cpp_sdk_asset(
            "org/repo",
            &release,
            &checksums,
            "0.1.0b8",
            &target(&["x86_64-unknown-linux-gnu", "x86_64-unknown-linux-musl"]),
        )
        .expect("legacy generic should resolve");
        assert!(selected.filename.contains("Linux-x86_64"));
        assert_eq!(selected.sha256, "legacy");
    }

    #[test]
    fn cpp_sdk_resolver_fails_when_checksum_missing() {
        let release = GithubRelease {
            tag_name: "v0.1.0b8".to_string(),
            name: "v0.1.0b8".to_string(),
            draft: false,
            prerelease: true,
            assets: vec![asset(
                "hackarena3-cpp-sdk-0.1.0b8-x86_64-pc-windows-msvc.tar.gz",
            )],
        };
        let checksums = HashMap::new();
        let err = resolve_cpp_sdk_asset(
            "org/repo",
            &release,
            &checksums,
            "0.1.0b8",
            &target(&["x86_64-pc-windows-msvc"]),
        )
        .expect_err("missing checksum should fail");
        assert!(err.to_string().contains("Checksum for asset"));
    }

    #[test]
    fn linux_resolver_prefers_first_matching_triple_and_fallbacks() {
        let assets = vec![
            asset("ha3-backend-local-x86_64-unknown-linux-gnu-v0.1.0.zip"),
            asset("ha3-backend-local-x86_64-unknown-linux-musl-v0.1.0.zip"),
        ];

        let selected = select_component_asset(
            &assets,
            ComponentSelector::Backend,
            &target(&["x86_64-unknown-linux-gnu", "x86_64-unknown-linux-musl"]),
        )
        .expect("gnu should be preferred");
        assert!(selected.name.contains("linux-gnu"));

        let selected = select_component_asset(
            &assets,
            ComponentSelector::Backend,
            &target(&["x86_64-unknown-linux-musl", "x86_64-unknown-linux-gnu"]),
        )
        .expect("musl should be preferred");
        assert!(selected.name.contains("linux-musl"));

        let assets = vec![asset(
            "ha3-backend-local-x86_64-unknown-linux-musl-v0.1.0.zip",
        )];
        let selected = select_component_asset(
            &assets,
            ComponentSelector::Backend,
            &target(&["x86_64-unknown-linux-gnu", "x86_64-unknown-linux-musl"]),
        )
        .expect("should fallback to musl");
        assert!(selected.name.contains("linux-musl"));
    }

    #[test]
    fn forced_linux_mode_without_asset_is_hard_fail() {
        let assets = vec![asset(
            "ha3-backend-local-x86_64-unknown-linux-musl-v0.1.0.zip",
        )];
        let err = select_component_asset(
            &assets,
            ComponentSelector::Backend,
            &target(&["x86_64-unknown-linux-gnu"]),
        )
        .expect_err("forced gnu should fail when only musl exists");
        assert!(err.to_string().contains("x86_64-unknown-linux-gnu"));
    }

    #[test]
    fn linux_mode_precedence_flag_env_default() {
        let from_flag =
            resolve_linux_mode_from_inputs(Some(LinuxLibcMode::Gnu), Some("musl"), "musl->gnu")
                .expect("flag mode");
        assert_eq!(from_flag.mode, LinuxLibcMode::Gnu);
        assert_eq!(from_flag.source, "flag");

        let from_env =
            resolve_linux_mode_from_inputs(None, Some("musl"), "gnu->musl").expect("env mode");
        assert_eq!(from_env.mode, LinuxLibcMode::Musl);
        assert_eq!(from_env.source, "env");

        let default_mode =
            resolve_linux_mode_from_inputs(None, None, "musl->gnu").expect("default mode");
        assert_eq!(default_mode.mode, LinuxLibcMode::Auto);
        assert_eq!(default_mode.source, "default");
    }

    #[test]
    fn linux_mode_invalid_env_is_rejected() {
        let err = resolve_linux_mode_from_inputs(None, Some("bad"), "musl->gnu")
            .expect_err("invalid env should fail");
        assert!(err.to_string().contains("HACKARENA_LINUX_LIBC"));
    }

    #[test]
    fn linux_triple_order_for_arch_follows_mode() {
        let (triples, _details) = linux_target_triples_for_arch(
            "x86_64",
            resolve_linux_mode_from_inputs(Some(LinuxLibcMode::Gnu), None, "musl->gnu")
                .expect("mode"),
        )
        .expect("triples");
        assert_eq!(triples, vec!["x86_64-unknown-linux-gnu"]);

        let (triples, _details) = linux_target_triples_for_arch(
            "x86_64",
            resolve_linux_mode_from_inputs(Some(LinuxLibcMode::Musl), None, "gnu->musl")
                .expect("mode"),
        )
        .expect("triples");
        assert_eq!(triples, vec!["x86_64-unknown-linux-musl"]);

        let (triples, _details) = linux_target_triples_for_arch(
            "x86_64",
            resolve_linux_mode_from_inputs(Some(LinuxLibcMode::Auto), None, "gnu->musl")
                .expect("mode"),
        )
        .expect("triples");
        assert_eq!(
            triples,
            vec!["x86_64-unknown-linux-gnu", "x86_64-unknown-linux-musl"]
        );
    }

    #[test]
    fn auth_binary_name_is_normalized_to_stable_command_name() {
        assert_eq!(
            super::infer_bin_name_auth("ha-auth-v0.2.0-x86_64-pc-windows-msvc.exe"),
            if cfg!(windows) {
                "ha-auth.exe".to_string()
            } else {
                "ha-auth".to_string()
            }
        );
    }

    #[test]
    fn release_selection_is_stable_only_without_flag() {
        let prerelease = GithubRelease {
            tag_name: "v0.1.0-beta.1".to_string(),
            name: "beta".to_string(),
            draft: false,
            prerelease: true,
            assets: vec![],
        };
        let stable = GithubRelease {
            tag_name: "v0.1.0".to_string(),
            name: "stable".to_string(),
            draft: false,
            prerelease: false,
            assets: vec![],
        };

        let selected = select_latest_release(&[stable.clone(), prerelease.clone()], false)
            .expect("release should be selected");
        assert_eq!(selected.tag_name, "v0.1.0");

        let selected = select_latest_release(&[prerelease], false);
        assert!(selected.is_none());
    }

    #[test]
    fn release_selection_allows_prerelease_with_flag() {
        let prerelease = GithubRelease {
            tag_name: "v0.1.0-beta.1".to_string(),
            name: "beta".to_string(),
            draft: false,
            prerelease: true,
            assets: vec![],
        };

        let selected =
            select_latest_release(&[prerelease], true).expect("fallback should select beta");
        assert_eq!(selected.tag_name, "v0.1.0-beta.1");
    }

    #[test]
    fn wrapper_instance_id_maps_to_base_id() {
        assert_eq!(wrapper_base_id("python"), "python");
        assert_eq!(wrapper_base_id("python_1"), "python");
        assert_eq!(wrapper_base_id("python_123"), "python");
        assert_eq!(wrapper_base_id("python_beta"), "python_beta");
        assert_eq!(wrapper_base_id("python_"), "python_");
    }
}
