use crate::config::{Paths, ensure_dir};
use crate::error::HackArenaError;
use reqwest::header::{ACCEPT, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

const GITHUB_AUTH_FILE: &str = "github.json";
const GITHUB_API_ACCEPT: &str = "application/vnd.github+json";
const GITHUB_TOKEN_VALIDATE_URL: &str = "https://api.github.com/rate_limit";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GithubTokenSource {
    Env,
    Stored,
}

impl GithubTokenSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::Stored => "stored",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedGithubToken {
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GithubAuthConfig {
    token: String,
}

pub async fn login(paths: &Paths) -> Result<(), HackArenaError> {
    let token = rpassword::prompt_password("GitHub token: ")?;
    let token = token.trim().to_string();
    if token.is_empty() {
        return Err(HackArenaError::msg("GitHub token cannot be empty."));
    }

    validate_github_token(&token).await?;

    ensure_dir(&paths.config_root())?;
    let path = github_auth_config_path(paths);
    let data = serde_json::to_vec_pretty(&GithubAuthConfig { token })
        .map_err(|e| HackArenaError::json_with_path(&path, e))?;
    fs::write(&path, data).map_err(|e| HackArenaError::io_with_path(&path, e))?;
    println!("Stored GitHub token.");
    Ok(())
}

pub fn logout(paths: &Paths) -> Result<(), HackArenaError> {
    let path = github_auth_config_path(paths);
    if !path.exists() {
        println!("No stored GitHub token.");
        return Ok(());
    }
    fs::remove_file(&path).map_err(|e| HackArenaError::io_with_path(&path, e))?;
    println!("Removed stored GitHub token.");
    Ok(())
}

pub fn status(paths: &Paths) -> Result<(), HackArenaError> {
    let source = github_token_source(paths)?
        .map(|source| source.as_str())
        .unwrap_or("none");
    println!("GitHub token source: {source}");
    Ok(())
}

pub fn github_token_source(paths: &Paths) -> Result<Option<GithubTokenSource>, HackArenaError> {
    if env_token().is_some() {
        return Ok(Some(GithubTokenSource::Env));
    }
    if load_stored_token(paths)?.is_some() {
        return Ok(Some(GithubTokenSource::Stored));
    }
    Ok(None)
}

pub fn resolve_github_token(paths: &Paths) -> Result<Option<ResolvedGithubToken>, HackArenaError> {
    if let Some(token) = env_token() {
        return Ok(Some(ResolvedGithubToken { token }));
    }
    if let Some(token) = load_stored_token(paths)? {
        return Ok(Some(ResolvedGithubToken { token }));
    }
    Ok(None)
}

fn github_auth_config_path(paths: &Paths) -> PathBuf {
    paths.config_root().join(GITHUB_AUTH_FILE)
}

fn env_token() -> Option<String> {
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

fn load_stored_token(paths: &Paths) -> Result<Option<String>, HackArenaError> {
    let path = github_auth_config_path(paths);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path).map_err(|e| HackArenaError::io_with_path(&path, e))?;
    let parsed: GithubAuthConfig =
        serde_json::from_slice(&bytes).map_err(|e| HackArenaError::json_with_path(&path, e))?;
    let token = parsed.token.trim().to_string();
    if token.is_empty() {
        return Ok(None);
    }
    Ok(Some(token))
}

async fn validate_github_token(token: &str) -> Result<(), HackArenaError> {
    let client = reqwest::Client::new();
    let resp = client
        .get(GITHUB_TOKEN_VALIDATE_URL)
        .header(USER_AGENT, "hackarena-cli")
        .header(ACCEPT, GITHUB_API_ACCEPT)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| HackArenaError::http_with_url(GITHUB_TOKEN_VALIDATE_URL, e))?;

    if resp.status().is_success() {
        return Ok(());
    }

    Err(HackArenaError::msg(format!(
        "GitHub rejected the token (HTTP {}). The token was not saved.",
        resp.status().as_u16()
    )))
}
