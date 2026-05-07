use crate::config::{Paths, ensure_dir};
use crate::error::HackArenaError;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

/// Downloads an artifact into the downloads cache directory and returns the cached file path.
///
/// Reuses an existing cached file when its SHA-256 matches `expected_sha256_hex`.
pub async fn download_to_cache(
    paths: &Paths,
    url: &str,
    cache_filename: &str,
    expected_sha256_hex: &str,
) -> Result<PathBuf, HackArenaError> {
    download_to_dir(
        &paths.downloads_cache_dir(),
        url,
        cache_filename,
        expected_sha256_hex,
    )
    .await
}

/// Downloads an artifact into an arbitrary directory and returns the local file path.
///
/// Reuses an existing file when its SHA-256 matches `expected_sha256_hex`.
pub async fn download_to_dir(
    target_dir: &Path,
    url: &str,
    cache_filename: &str,
    expected_sha256_hex: &str,
) -> Result<PathBuf, HackArenaError> {
    ensure_dir(target_dir)?;

    let cache_path = target_dir.join(cache_filename);
    let tmp_path = target_dir.join(format!("{cache_filename}.partial"));

    if cache_path.is_file() {
        let cached_sha = sha256_file_hex(&cache_path)?;
        if eq_hex_sha256(expected_sha256_hex, &cached_sha) {
            println!("Using cached `{cache_filename}`.");
            return Ok(cache_path);
        }
        tokio::fs::remove_file(&cache_path)
            .await
            .map_err(|e| HackArenaError::io_with_path(&cache_path, e))?;
    }

    let client = reqwest::Client::new();
    for attempt in 1..=2 {
        if tmp_path.exists() {
            let _ = tokio::fs::remove_file(&tmp_path).await;
        }

        let resp = get_with_token_fallback(&client, url).await?;
        let final_url = resp.url().to_string();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        if content_type
            .as_deref()
            .is_some_and(|ct| ct.starts_with("text/") || ct.contains("html") || ct.contains("json"))
        {
            let bytes = resp
                .bytes()
                .await
                .map_err(|e| HackArenaError::http_with_url(&final_url, e))?;
            let snippet = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_string();
            return Err(HackArenaError::msg(format!(
                "expected a binary artifact but got `{}` from `{final_url}` (original `{url}`). First bytes:\n{snippet}",
                content_type.unwrap_or_else(|| "<unknown content-type>".to_string())
            )));
        }

        let mut file = tokio::fs::File::create(&tmp_path)
            .await
            .map_err(|e| HackArenaError::io_with_path(&tmp_path, e))?;

        let mut hasher = Sha256::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(|e| HackArenaError::http_with_url(&final_url, e))?;
            hasher.update(&bytes);
            file.write_all(&bytes)
                .await
                .map_err(|e| HackArenaError::io_with_path(&tmp_path, e))?;
        }
        file.flush()
            .await
            .map_err(|e| HackArenaError::io_with_path(&tmp_path, e))?;

        let actual = hex::encode(hasher.finalize());
        if !eq_hex_sha256(expected_sha256_hex, &actual) {
            if attempt == 1 {
                continue;
            }
            return Err(HackArenaError::ChecksumMismatch {
                path: tmp_path,
                expected: expected_sha256_hex.to_string(),
                actual,
            });
        }

        break;
    }

    // Atomic-ish replace on most platforms.
    if cache_path.exists() {
        tokio::fs::remove_file(&cache_path)
            .await
            .map_err(|e| HackArenaError::io_with_path(&cache_path, e))?;
    }
    tokio::fs::rename(&tmp_path, &cache_path)
        .await
        .map_err(|e| HackArenaError::io_with_path(&cache_path, e))?;

    Ok(cache_path)
}

/// Computes SHA-256 of a file on disk, returning lowercase hex.
pub fn sha256_file_hex(path: &Path) -> Result<String, HackArenaError> {
    let mut file = std::fs::File::open(path).map_err(|e| HackArenaError::io_with_path(path, e))?;
    let mut buffer = [0u8; 64 * 1024];
    let mut hasher = Sha256::new();
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|e| HackArenaError::io_with_path(path, e))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn eq_hex_sha256(expected: &str, actual: &str) -> bool {
    expected.trim().eq_ignore_ascii_case(actual.trim())
}

async fn get_with_token_fallback(
    client: &reqwest::Client,
    url: &str,
) -> Result<reqwest::Response, HackArenaError> {
    let token = github_token();
    let token_present = token.is_some();
    let resp = send_get(client, url, token.as_deref()).await?;
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

async fn send_get(
    client: &reqwest::Client,
    url: &str,
    token: Option<&str>,
) -> Result<reqwest::Response, HackArenaError> {
    let mut req = client
        .get(url)
        .header(reqwest::header::USER_AGENT, "hackarena-cli");
    if is_release_asset_api_url(url) {
        req = req.header(reqwest::header::ACCEPT, "application/octet-stream");
    }
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

fn github_http_status_error(
    url: &str,
    status: reqwest::StatusCode,
    token_present: bool,
) -> HackArenaError {
    if status == reqwest::StatusCode::NOT_FOUND
        && (url.contains("github.com/") || url.contains("githubusercontent.com/"))
    {
        if token_present {
            return HackArenaError::msg(format!(
                "GitHub returned 404 for `{url}`. The release asset may not exist or GH_TOKEN/GITHUB_TOKEN lacks access."
            ));
        }
        return HackArenaError::msg(format!(
            "GitHub returned 404 for `{url}`. For private repositories this is expected without authentication. Set GH_TOKEN (or GITHUB_TOKEN) with repo read access."
        ));
    }
    HackArenaError::msg(format!("HTTP {} for `{url}`", status.as_u16()))
}
