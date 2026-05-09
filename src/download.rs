use crate::config::{Paths, ensure_dir};
use crate::error::HackArenaError;
use crate::github_http::{self, GITHUB_BINARY_ACCEPT, GithubGetOutcome};
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
        paths,
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
    paths: &Paths,
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

        let resp =
            match github_http::get(paths, &client, url, accept_for_url(url), None, false).await? {
                GithubGetOutcome::Response(resp) => resp,
                GithubGetOutcome::RateLimited(info) => {
                    return Err(github_http::rate_limit_error(url, info.status_code));
                }
                GithubGetOutcome::NotModified => {
                    return Err(HackArenaError::msg(format!(
                        "GitHub unexpectedly returned 304 for `{url}` without an ETag request."
                    )));
                }
                GithubGetOutcome::NotFound => {
                    return Err(github_http::github_http_status_error(
                        url,
                        reqwest::StatusCode::NOT_FOUND,
                        false,
                    ));
                }
            };
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

fn accept_for_url(url: &str) -> &'static str {
    if is_release_asset_api_url(url) {
        GITHUB_BINARY_ACCEPT
    } else {
        github_http::GITHUB_JSON_ACCEPT
    }
}

fn is_release_asset_api_url(url: &str) -> bool {
    url.contains("api.github.com/repos/") && url.contains("/releases/assets/")
}
