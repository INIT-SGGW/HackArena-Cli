use crate::auth_cmd::resolve_auth_binary;
use crate::cmd_hint;
use crate::config::{Paths, is_project_dir, load_project_config};
use crate::constants::PROJECT_WRAPPERS_DIR;
use crate::error::HackArenaError;
use crate::submission_proto::submission_v1::{
    SubmitBuildRequest, SubmitBuildStreamResponse,
    submit_build_stream_response::Event as SubmitEvent,
};
use flate2::Compression;
use flate2::write::GzEncoder;
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use std::fs;
use std::io::{self, Cursor, IsTerminal, Write};
use std::path::{Path, PathBuf};
use tonic::client::Grpc;
use tonic::codegen::http::uri::PathAndQuery;
use tonic::metadata::MetadataValue;

const DEFAULT_API_URL: &str = "https://ha3-api.hackarena.pl";
const API_URL_ENV_KEY: &str = "HA3_WRAPPER_API_URL";
const RUNTIME_MARKER: &str = "# hackarena3-runtime: managed by hackarena-cli";
const USER_HINT: &str = "# add your own dependencies below";

pub async fn submit(
    paths: &Paths,
    slot: Option<u8>,
    description: Option<&str>,
) -> Result<(), HackArenaError> {
    let cwd = std::env::current_dir().map_err(HackArenaError::Io)?;
    if !is_project_dir(&cwd) {
        return Err(HackArenaError::msg(format!(
            "No project found. Run `{}` first.",
            cmd_hint::run_cli("use <edition>")
        )));
    }
    let project = load_project_config(&cwd)?;
    let slot = resolve_submit_slot(&project.edition, slot)?;

    let wrappers_root = cwd.join(PROJECT_WRAPPERS_DIR);
    let wrappers = discover_wrappers(&wrappers_root)?;
    if wrappers.is_empty() {
        return Err(HackArenaError::msg(format!(
            "No wrappers with `system/manifest.toml` found in {}.",
            wrappers_root.display()
        )));
    }

    let selected = choose_wrapper(&wrappers)?;
    let manifest_path = selected.dir.join("system").join("manifest.toml");
    let manifest = load_wrapper_manifest(&manifest_path)?;
    let wrapper_kind = map_wrapper_kind(&manifest.wrapper.language)?;
    let wrapper_version = manifest.wrapper.template_version.clone().ok_or_else(|| {
        HackArenaError::msg(format!(
            "Wrapper `{}` manifest is missing `wrapper.template_version`.",
            selected.id
        ))
    })?;
    let submit = manifest.submit.ok_or_else(|| {
        HackArenaError::msg(format!(
            "Wrapper `{}` manifest is missing `[submit]` section.",
            selected.id
        ))
    })?;
    let include = submit.include.ok_or_else(|| {
        HackArenaError::msg(format!(
            "Wrapper `{}` manifest is missing `[submit].include`.",
            selected.id
        ))
    })?;
    let exclude = submit.exclude.ok_or_else(|| {
        HackArenaError::msg(format!(
            "Wrapper `{}` manifest is missing `[submit].exclude`.",
            selected.id
        ))
    })?;
    if include.is_empty() {
        return Err(HackArenaError::msg(format!(
            "Wrapper `{}` has empty `[submit].include`.",
            selected.id
        )));
    }

    let files = collect_submission_files(&selected.dir, &include, &exclude)?;
    if files.is_empty() {
        return Err(HackArenaError::msg(format!(
            "Wrapper `{}` submit archive is empty after applying include/exclude rules.",
            selected.id
        )));
    }
    let archive = build_tar_gz(&files)?;

    let token = fetch_jwt(paths)?;
    let api_url = resolve_api_url(&selected.dir)?;
    let transport_endpoint = submission_transport_endpoint_from_api_url(&api_url);
    let rpc_path = submission_rpc_path_from_api_url(&api_url)?;
    println!("wrapper: {}", selected.id);
    if let Some(slot) = slot {
        println!("slot: {}", slot);
    }
    if let Some(description) = description.filter(|value| !value.trim().is_empty()) {
        println!("description: {}", description.trim());
    }

    let result = submit_build(
        &transport_endpoint,
        &rpc_path,
        &token,
        wrapper_kind,
        &wrapper_version,
        slot,
        description,
        archive,
    )
    .await?;

    println!("submission_id: {}", result.submission_id);
    if result.success {
        println!("build: succeeded");
        return Ok(());
    }

    println!("build: failed");
    Err(HackArenaError::msg(format!(
        "Build failed for submission `{}`.",
        result.submission_id
    )))
}

struct SubmitBuildResult {
    submission_id: String,
    success: bool,
}

fn rpc_failed_error(endpoint: &str, rpc_path: &PathAndQuery, message: &str) -> HackArenaError {
    HackArenaError::msg(format!(
        "SubmitBuildStream RPC failed for `{endpoint}` at path `{}`: {}",
        rpc_path, message
    ))
}

fn parse_stream_event(
    msg: SubmitBuildStreamResponse,
    endpoint: &str,
    rpc_path: &PathAndQuery,
    submission_id: &mut Option<String>,
    finished: &mut Option<bool>,
) -> Result<(), HackArenaError> {
    match msg.event {
        Some(SubmitEvent::Started(started)) => {
            if started.submission_id.trim().is_empty() {
                return Err(rpc_failed_error(
                    endpoint,
                    rpc_path,
                    "received `BuildStarted` with empty `submission_id`",
                ));
            }
            *submission_id = Some(started.submission_id);
        }
        Some(SubmitEvent::Log(log)) => {
            println!("{}", log.line);
        }
        Some(SubmitEvent::Finished(done)) => {
            *finished = Some(done.success);
        }
        None => {
            return Err(rpc_failed_error(
                endpoint,
                rpc_path,
                "received response with empty event payload",
            ));
        }
    }

    Ok(())
}

fn resolve_submit_slot(
    edition: &str,
    requested_slot: Option<u8>,
) -> Result<Option<u8>, HackArenaError> {
    if edition == "3" && requested_slot.is_none() {
        return Err(HackArenaError::msg(
            "Edition `3` requires `--slot` with value `1`, `2` or `3` (e.g. `hackarena submit --slot 1`).",
        ));
    }
    Ok(requested_slot)
}

#[derive(Debug, Clone)]
struct WrapperCandidate {
    id: String,
    dir: PathBuf,
}

fn discover_wrappers(wrappers_root: &Path) -> Result<Vec<WrapperCandidate>, HackArenaError> {
    if !wrappers_root.exists() {
        return Ok(vec![]);
    }

    let mut wrappers = Vec::<WrapperCandidate>::new();
    let rd =
        fs::read_dir(wrappers_root).map_err(|e| HackArenaError::io_with_path(wrappers_root, e))?;
    for entry in rd {
        let entry = entry.map_err(HackArenaError::Io)?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| HackArenaError::io_with_path(&path, e))?;
        if !file_type.is_dir() {
            continue;
        }
        let Some(id) = path.file_name().and_then(|v| v.to_str()) else {
            continue;
        };
        let manifest_path = path.join("system").join("manifest.toml");
        if !manifest_path.is_file() {
            println!(
                "Skipping wrapper `{}`: missing `{}`.",
                id,
                manifest_path.display()
            );
            continue;
        }
        wrappers.push(WrapperCandidate {
            id: id.to_string(),
            dir: path,
        });
    }
    wrappers.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(wrappers)
}

fn choose_wrapper(wrappers: &[WrapperCandidate]) -> Result<WrapperCandidate, HackArenaError> {
    if wrappers.len() == 1 {
        println!("Using wrapper `{}`.", wrappers[0].id);
        return Ok(wrappers[0].clone());
    }

    if !(io::stdin().is_terminal() && io::stdout().is_terminal()) {
        return Err(HackArenaError::msg(
            "Multiple wrappers found. Run `hackarena submit` in an interactive terminal.",
        ));
    }

    println!("Available wrappers:");
    for (idx, wrapper) in wrappers.iter().enumerate() {
        println!("  {}. {}", idx + 1, wrapper.id);
    }
    print!("Choose wrapper number: ");
    io::stdout().flush().map_err(HackArenaError::Io)?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(HackArenaError::Io)?;
    let index = input
        .trim()
        .parse::<usize>()
        .map_err(|_| HackArenaError::msg("Invalid wrapper number."))?;
    if index == 0 || index > wrappers.len() {
        return Err(HackArenaError::msg(
            "Selected wrapper number is out of range.",
        ));
    }
    Ok(wrappers[index - 1].clone())
}

#[derive(Debug, Deserialize)]
struct WrapperManifest {
    wrapper: ManifestWrapperSection,
    submit: Option<ManifestSubmitSection>,
}

#[derive(Debug, Deserialize)]
struct ManifestWrapperSection {
    language: String,
    template_version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ManifestSubmitSection {
    include: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
}

fn load_wrapper_manifest(path: &Path) -> Result<WrapperManifest, HackArenaError> {
    let text = fs::read_to_string(path).map_err(|e| HackArenaError::io_with_path(path, e))?;
    toml::from_str::<WrapperManifest>(&text).map_err(|e| {
        HackArenaError::msg(format!(
            "Invalid wrapper manifest at {}: {}",
            path.display(),
            e
        ))
    })
}

#[derive(Copy, Clone, Debug)]
enum WrapperKindMapped {
    Python = 1,
    Csharp = 2,
    Cpp = 3,
}

fn map_wrapper_kind(language: &str) -> Result<WrapperKindMapped, HackArenaError> {
    let normalized = language.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "python" => Ok(WrapperKindMapped::Python),
        "csharp" => Ok(WrapperKindMapped::Csharp),
        "cpp" => Ok(WrapperKindMapped::Cpp),
        _ => Err(HackArenaError::msg(format!(
            "Wrapper language `{language}` is not implemented yet for submit (supported: `python`, `csharp`, `cpp`)."
        ))),
    }
}

#[derive(Debug)]
struct PatternMatcher {
    literals: Vec<String>,
    globset: Option<GlobSet>,
}

impl PatternMatcher {
    fn compile(patterns: &[String], field_name: &str) -> Result<Self, HackArenaError> {
        let mut builder = GlobSetBuilder::new();
        let mut literals = Vec::<String>::new();
        let mut has_globs = false;

        for raw in patterns {
            let normalized = normalize_pattern(raw);
            if normalized.is_empty() {
                continue;
            }
            if has_glob_meta(&normalized) {
                let glob = Glob::new(&normalized).map_err(|e| {
                    HackArenaError::msg(format!("Invalid glob in {field_name}: `{raw}` ({e})"))
                })?;
                builder.add(glob);
                has_globs = true;
            } else {
                literals.push(normalized.trim_end_matches('/').to_string());
            }
        }

        let globset = if has_globs {
            Some(builder.build().map_err(|e| {
                HackArenaError::msg(format!("Invalid glob set for {field_name}: {e}"))
            })?)
        } else {
            None
        };

        Ok(Self { literals, globset })
    }

    fn matches(&self, rel: &str) -> bool {
        let rel = normalize_pattern(rel);
        if rel.is_empty() {
            return false;
        }
        if self
            .literals
            .iter()
            .any(|lit| rel == *lit || rel.starts_with(&format!("{lit}/")))
        {
            return true;
        }
        self.globset.as_ref().is_some_and(|g| g.is_match(&rel))
    }
}

fn collect_submission_files(
    wrapper_dir: &Path,
    include: &[String],
    exclude: &[String],
) -> Result<Vec<(PathBuf, String)>, HackArenaError> {
    let include_matcher = PatternMatcher::compile(include, "[submit].include")?;
    let exclude_matcher = PatternMatcher::compile(exclude, "[submit].exclude")?;

    let mut files = Vec::<PathBuf>::new();
    collect_files_recursive(wrapper_dir, &mut files)?;

    let mut selected = Vec::<(PathBuf, String)>::new();
    for abs in files {
        let rel = abs.strip_prefix(wrapper_dir).map_err(|_| {
            HackArenaError::msg(format!(
                "Failed to compute relative path for `{}`.",
                abs.display()
            ))
        })?;
        let rel_string = normalize_pattern(&rel.to_string_lossy());
        if !include_matcher.matches(&rel_string) {
            continue;
        }
        if exclude_matcher.matches(&rel_string) {
            continue;
        }

        let archive_rel = rel_string
            .strip_prefix("user/")
            .ok_or_else(|| {
                HackArenaError::msg(format!(
                    "Selected submit path `{}` is outside `user/`. Update `[submit].include`/`exclude` in `system/manifest.toml`.",
                    rel_string
                ))
            })?
            .to_string();

        if archive_rel.is_empty() {
            continue;
        }
        selected.push((abs, archive_rel));
    }

    selected.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(selected)
}

fn collect_files_recursive(root: &Path, out: &mut Vec<PathBuf>) -> Result<(), HackArenaError> {
    let rd = fs::read_dir(root).map_err(|e| HackArenaError::io_with_path(root, e))?;
    for entry in rd {
        let entry = entry.map_err(HackArenaError::Io)?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| HackArenaError::io_with_path(&path, e))?;
        if file_type.is_dir() {
            collect_files_recursive(&path, out)?;
            continue;
        }
        if file_type.is_file() {
            out.push(path);
        }
    }
    Ok(())
}

fn build_tar_gz(entries: &[(PathBuf, String)]) -> Result<Vec<u8>, HackArenaError> {
    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut tar = tar::Builder::new(encoder);
    for (abs, rel) in entries {
        if rel == "requirements.txt" {
            let sanitized = sanitize_requirements_for_submit(abs)?;
            append_bytes_to_tar(&mut tar, rel, &sanitized)?;
            continue;
        }
        tar.append_path_with_name(abs, rel)
            .map_err(|e| HackArenaError::io_with_path(abs, e))?;
    }
    let encoder = tar.into_inner().map_err(HackArenaError::Io)?;
    encoder.finish().map_err(HackArenaError::Io)
}

fn append_bytes_to_tar(
    tar: &mut tar::Builder<GzEncoder<Vec<u8>>>,
    rel: &str,
    bytes: &[u8],
) -> Result<(), HackArenaError> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, rel, Cursor::new(bytes))
        .map_err(|e| HackArenaError::io_with_path(Path::new(rel), e))?;
    Ok(())
}

fn sanitize_requirements_for_submit(path: &Path) -> Result<Vec<u8>, HackArenaError> {
    let content = fs::read_to_string(path).map_err(|e| HackArenaError::io_with_path(path, e))?;
    let mut lines: Vec<String> = Vec::new();
    for line in content.lines() {
        if is_hackarena3_requirement_comment(line)
            || is_managed_hackarena3_wheel_line(line)
            || is_user_requirements_hint_comment(line)
        {
            continue;
        }
        lines.push(line.to_string());
    }

    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    Ok(out.into_bytes())
}

fn is_hackarena3_requirement_comment(line: &str) -> bool {
    line.trim().starts_with(RUNTIME_MARKER)
}

fn is_user_requirements_hint_comment(line: &str) -> bool {
    line.trim().eq_ignore_ascii_case(USER_HINT)
}

fn is_managed_hackarena3_wheel_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    if (lower.starts_with("./.vendor/hackarena3-")
        || lower.starts_with("./user/.vendor/hackarena3-"))
        && lower.ends_with(".whl")
    {
        return true;
    }
    (lower.contains("hackarena3.0-apiwrapper-python") && lower.contains("hackarena3-"))
        && lower.ends_with(".whl")
}

fn normalize_pattern(value: &str) -> String {
    value
        .trim()
        .replace('\\', "/")
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_string()
}

fn has_glob_meta(pattern: &str) -> bool {
    pattern
        .chars()
        .any(|c| matches!(c, '*' | '?' | '[' | ']' | '{' | '}'))
}

fn fetch_jwt(paths: &Paths) -> Result<String, HackArenaError> {
    let auth_bin = resolve_auth_binary(paths)?;
    let output = std::process::Command::new(&auth_bin)
        .args(["token", "--raw", "-q"])
        .output()
        .map_err(|e| HackArenaError::io_with_path(&auth_bin, e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let details = if stderr.is_empty() {
            "ha-auth token failed.".to_string()
        } else {
            format!("ha-auth token failed: {stderr}")
        };
        return Err(HackArenaError::msg(format!(
            "{details} Run `{}` first.",
            cmd_hint::run_cli("auth login")
        )));
    }

    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if token.is_empty() {
        return Err(HackArenaError::msg(format!(
            "Empty token returned by ha-auth. Run `{}` first.",
            cmd_hint::run_cli("auth login")
        )));
    }
    Ok(token)
}

fn resolve_api_url(wrapper_dir: &Path) -> Result<String, HackArenaError> {
    if let Ok(from_process_env) = std::env::var(API_URL_ENV_KEY) {
        let trimmed = from_process_env.trim();
        if !trimmed.is_empty() {
            return normalize_api_url(trimmed);
        }
    }

    let env_path = wrapper_dir.join("user").join(".env");
    let from_env = if env_path.is_file() {
        let content = fs::read_to_string(&env_path)
            .map_err(|e| HackArenaError::io_with_path(&env_path, e))?;
        read_env_key(&content, API_URL_ENV_KEY)
    } else {
        None
    };

    let raw = from_env.unwrap_or_else(|| DEFAULT_API_URL.to_string());
    normalize_api_url(&raw)
}

fn read_env_key(content: &str, key: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((left, right)) = trimmed.split_once('=') else {
            continue;
        };
        if left.trim() != key {
            continue;
        }
        let value = right.trim().trim_matches('"').trim_matches('\'').trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn normalize_api_url(value: &str) -> Result<String, HackArenaError> {
    let mut url = value.trim().to_string();
    if url.is_empty() {
        url = DEFAULT_API_URL.to_string();
    }
    if !url.starts_with("https://") && !url.starts_with("http://") {
        url = format!("https://{url}");
    }

    let parsed = reqwest::Url::parse(&url)
        .map_err(|e| HackArenaError::msg(format!("Invalid API URL `{url}`: {e}")))?;
    if parsed.scheme() != "https" {
        return Err(HackArenaError::msg(format!(
            "Only `https://` API URL is supported for submit, got `{}`.",
            parsed
        )));
    }

    Ok(parsed.to_string().trim_end_matches('/').to_string())
}

fn submission_transport_endpoint_from_api_url(api_url: &str) -> String {
    match reqwest::Url::parse(api_url) {
        Ok(mut parsed) => {
            parsed.set_path("");
            parsed.set_query(None);
            parsed.set_fragment(None);
            parsed.to_string().trim_end_matches('/').to_string()
        }
        Err(_) => api_url.to_string(),
    }
}

fn submission_rpc_path_from_api_url(api_url: &str) -> Result<PathAndQuery, HackArenaError> {
    let parsed = reqwest::Url::parse(api_url)
        .map_err(|e| HackArenaError::msg(format!("Invalid API URL `{api_url}`: {e}")))?;
    let base = parsed.path().trim_end_matches('/');
    let prefix = if base.is_empty() {
        "/backend"
    } else if base.ends_with("/backend") {
        base
    } else {
        return Err(HackArenaError::msg(format!(
            "API URL path must be empty or end with `/backend`, got `{}`.",
            parsed.path()
        )));
    };
    let path = format!("{prefix}/submission.v1.SubmissionService/SubmitBuildStream");
    PathAndQuery::from_maybe_shared(path).map_err(|_| {
        HackArenaError::msg(
            "Failed to build gRPC path `/backend/submission.v1.SubmissionService/SubmitBuildStream`.",
        )
    })
}

async fn submit_build(
    endpoint: &str,
    rpc_path: &PathAndQuery,
    jwt: &str,
    wrapper_kind: WrapperKindMapped,
    wrapper_version: &str,
    slot: Option<u8>,
    description: Option<&str>,
    archive: Vec<u8>,
) -> Result<SubmitBuildResult, HackArenaError> {
    let parsed_endpoint = reqwest::Url::parse(endpoint).map_err(|e| {
        HackArenaError::msg(format!("Invalid submission endpoint `{endpoint}`: {e}"))
    })?;
    let host = parsed_endpoint
        .host_str()
        .ok_or_else(|| {
            HackArenaError::msg(format!("Invalid submission endpoint host in `{endpoint}`."))
        })?
        .to_string();
    let tls = tonic::transport::ClientTlsConfig::new()
        .domain_name(host)
        .with_native_roots();

    let channel = tonic::transport::Endpoint::from_shared(endpoint.to_string())
        .map_err(|e| HackArenaError::msg(format!("Invalid submission endpoint `{endpoint}`: {e}")))?
        .tls_config(tls)
        .map_err(|e| HackArenaError::msg(format!("Cannot configure TLS for `{endpoint}`: {e}")))?
        .connect()
        .await
        .map_err(|e| {
            HackArenaError::msg(format!(
                "Cannot connect to submission backend `{endpoint}`: {e}"
            ))
        })?;
    let mut client = Grpc::new(channel);

    let description = description.unwrap_or("").trim().to_string();
    let mut request = tonic::Request::new(SubmitBuildRequest {
        wrapper_kind: wrapper_kind as i32,
        wrapper_version: wrapper_version.to_string(),
        user_archive_tar_gz: archive,
        slot: slot.map(i32::from),
        description,
    });
    let auth_value = MetadataValue::try_from(format!("Bearer {jwt}"))
        .map_err(|_| HackArenaError::msg("Failed to build `authorization` metadata."))?;
    let jwt_value = MetadataValue::try_from(jwt.to_string())
        .map_err(|_| HackArenaError::msg("Failed to build `jwt` metadata."))?;
    let cookie_value = MetadataValue::try_from(format!("auth_token={jwt}"))
        .map_err(|_| HackArenaError::msg("Failed to build `cookie` metadata."))?;
    request.metadata_mut().insert("authorization", auth_value);
    request.metadata_mut().insert("jwt", jwt_value);
    request.metadata_mut().insert("cookie", cookie_value);

    client.ready().await.map_err(|e| {
        HackArenaError::msg(format!(
            "Submission client is not ready for `{endpoint}`: {e}"
        ))
    })?;

    let response = client
        .server_streaming(
            request,
            rpc_path.clone(),
            tonic::codec::ProstCodec::default(),
        )
        .await
        .map_err(|e| rpc_failed_error(endpoint, rpc_path, e.message()))?;

    let mut stream = response.into_inner();
    let mut submission_id: Option<String> = None;
    let mut finished: Option<bool> = None;

    println!("build logs:");
    while let Some(msg) = stream
        .message()
        .await
        .map_err(|e| rpc_failed_error(endpoint, rpc_path, e.message()))?
    {
        parse_stream_event(msg, endpoint, rpc_path, &mut submission_id, &mut finished)?;
        if finished.is_some() {
            break;
        }
    }

    let success = finished.ok_or_else(|| {
        rpc_failed_error(
            endpoint,
            rpc_path,
            "stream ended before `BuildFinished` event was received",
        )
    })?;
    let submission_id = submission_id.ok_or_else(|| {
        rpc_failed_error(
            endpoint,
            rpc_path,
            "stream ended without `BuildStarted` event",
        )
    })?;

    Ok(SubmitBuildResult {
        submission_id,
        success,
    })
}

#[cfg(test)]
mod tests {
    use super::map_wrapper_kind;

    #[test]
    fn map_wrapper_kind_supports_cpp() {
        let kind = map_wrapper_kind("cpp").expect("cpp should be supported");
        assert_eq!(kind as i32, 3);
    }
}
