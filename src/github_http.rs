use crate::config::Paths;
use crate::error::HackArenaError;
use crate::github_auth::resolve_github_token;
use reqwest::header::{ACCEPT, ETAG, HeaderMap, IF_NONE_MATCH, RETRY_AFTER, USER_AGENT};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const GITHUB_JSON_ACCEPT: &str = "application/vnd.github+json";
pub const GITHUB_BINARY_ACCEPT: &str = "application/octet-stream";

#[derive(Debug, Clone)]
pub struct GithubRateLimitInfo {
    pub status_code: u16,
    pub retry_after: Option<Duration>,
    pub reset_after: Option<Duration>,
}

pub enum GithubGetOutcome {
    Response(reqwest::Response),
    NotModified,
    NotFound,
    RateLimited(GithubRateLimitInfo),
}

enum GithubResponseAction {
    Return(GithubGetOutcome),
    RetryAnonymous,
    Error(HackArenaError),
}

pub async fn get(
    paths: &Paths,
    client: &reqwest::Client,
    url: &str,
    accept: &str,
    if_none_match: Option<&str>,
    allow_not_found: bool,
) -> Result<GithubGetOutcome, HackArenaError> {
    let resolved = resolve_github_token(paths)?;
    let token = resolved.as_ref().map(|value| value.token.as_str());
    let token_present = token.is_some();

    let resp = send_get(client, url, accept, if_none_match, token).await?;
    match classify_response(url, resp, token_present, allow_not_found)? {
        GithubResponseAction::Return(outcome) => Ok(outcome),
        GithubResponseAction::RetryAnonymous => {
            let anon = send_get(client, url, accept, if_none_match, None).await?;
            match classify_response(url, anon, false, allow_not_found)? {
                GithubResponseAction::Return(outcome) => Ok(outcome),
                GithubResponseAction::RetryAnonymous => Err(HackArenaError::msg(format!(
                    "GitHub request for `{url}` unexpectedly asked for anonymous retry twice."
                ))),
                GithubResponseAction::Error(err) => Err(err),
            }
        }
        GithubResponseAction::Error(err) => Err(err),
    }
}

pub fn rate_limit_error(url: &str, status_code: u16) -> HackArenaError {
    HackArenaError::msg(format!(
        "GitHub rate limit reached for `{url}` (HTTP {status_code}). Run `hackarena github login` or set GH_TOKEN/GITHUB_TOKEN."
    ))
}

pub fn github_http_status_error(
    url: &str,
    status: reqwest::StatusCode,
    token_present: bool,
) -> HackArenaError {
    if status == reqwest::StatusCode::NOT_FOUND {
        if token_present {
            return HackArenaError::msg(format!(
                "GitHub returned 404 for `{url}`. The repository or release asset may not exist, or the configured token does not have access."
            ));
        }
        return HackArenaError::msg(format!(
            "GitHub returned 404 for `{url}`. For private repositories this is expected without authentication. Run `hackarena github login` or set GH_TOKEN/GITHUB_TOKEN with repo read access."
        ));
    }

    HackArenaError::msg(format!("HTTP {} for `{url}`", status.as_u16()))
}

pub fn response_etag(headers: &HeaderMap) -> Option<String> {
    headers
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_string())
}

async fn send_get(
    client: &reqwest::Client,
    url: &str,
    accept: &str,
    if_none_match: Option<&str>,
    token: Option<&str>,
) -> Result<reqwest::Response, HackArenaError> {
    let mut req = client
        .get(url)
        .header(USER_AGENT, "hackarena-cli")
        .header(ACCEPT, accept);
    if let Some(etag) = if_none_match {
        req = req.header(IF_NONE_MATCH, etag);
    }
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }
    req.send()
        .await
        .map_err(|e| HackArenaError::http_with_url(url, e))
}

fn classify_response(
    url: &str,
    resp: reqwest::Response,
    token_present: bool,
    allow_not_found: bool,
) -> Result<GithubResponseAction, HackArenaError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(GithubResponseAction::Return(GithubGetOutcome::Response(
            resp,
        )));
    }
    if status == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(GithubResponseAction::Return(GithubGetOutcome::NotModified));
    }
    if status == reqwest::StatusCode::NOT_FOUND && allow_not_found {
        return Ok(GithubResponseAction::Return(GithubGetOutcome::NotFound));
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Ok(GithubResponseAction::Return(GithubGetOutcome::RateLimited(
            parse_rate_limit_info(resp.headers(), status.as_u16()),
        )));
    }
    if status == reqwest::StatusCode::UNAUTHORIZED && token_present {
        return Ok(GithubResponseAction::RetryAnonymous);
    }
    if status == reqwest::StatusCode::FORBIDDEN {
        if is_rate_limited(resp.headers()) {
            return Ok(GithubResponseAction::Return(GithubGetOutcome::RateLimited(
                parse_rate_limit_info(resp.headers(), status.as_u16()),
            )));
        }
        if token_present {
            return Ok(GithubResponseAction::RetryAnonymous);
        }
    }

    Ok(GithubResponseAction::Error(github_http_status_error(
        url,
        status,
        token_present,
    )))
}

fn is_rate_limited(headers: &HeaderMap) -> bool {
    if headers.get(RETRY_AFTER).is_some() {
        return true;
    }
    headers
        .get("x-ratelimit-remaining")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim() == "0")
}

fn parse_rate_limit_info(headers: &HeaderMap, status_code: u16) -> GithubRateLimitInfo {
    let retry_after = headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs);

    let reset_after = headers
        .get("x-ratelimit-reset")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .and_then(|reset_unix| {
            let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
            Some(Duration::from_secs(reset_unix.saturating_sub(now)))
        });

    GithubRateLimitInfo {
        status_code,
        retry_after,
        reset_after,
    }
}
