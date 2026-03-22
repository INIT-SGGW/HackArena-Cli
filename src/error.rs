use std::path::PathBuf;

/// Errors returned by the `hackarena` CLI.
#[derive(thiserror::Error, Debug)]
pub enum HackArenaError {
    #[error("{0}")]
    Message(String),

    #[error("I/O error while working with {path}: {source}")]
    IoWithPath {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error while working with {path}: {source}")]
    JsonWithPath {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("JSON error while working with {url}: {source}")]
    JsonWithUrl {
        url: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("JSON error while working with {context}: {source}")]
    JsonWithContext {
        context: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("HTTP error for {url}: {source}")]
    HttpWithUrl {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("ZIP error while working with {path}: {source}")]
    ZipWithPath {
        path: PathBuf,
        #[source]
        source: zip::result::ZipError,
    },

    #[error("unsupported archive format: {path}")]
    ArchiveFormat { path: PathBuf },

    #[error("unknown wrapper id `{0}` in edition config")]
    UnknownWrapper(String),

    #[error("checksum mismatch for {path}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
}

impl HackArenaError {
    pub fn msg(msg: impl Into<String>) -> Self {
        Self::Message(msg.into())
    }

    pub fn io_with_path(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::IoWithPath {
            path: path.into(),
            source,
        }
    }

    pub fn json_with_path(path: impl Into<PathBuf>, source: serde_json::Error) -> Self {
        Self::JsonWithPath {
            path: path.into(),
            source,
        }
    }

    pub fn json_with_url(url: impl Into<String>, source: serde_json::Error) -> Self {
        Self::JsonWithUrl {
            url: url.into(),
            source,
        }
    }

    pub fn json_with_context(context: impl Into<String>, source: serde_json::Error) -> Self {
        Self::JsonWithContext {
            context: context.into(),
            source,
        }
    }

    pub fn http_with_url(url: impl Into<String>, source: reqwest::Error) -> Self {
        Self::HttpWithUrl {
            url: url.into(),
            source,
        }
    }

    pub fn zip_with_path(path: impl Into<PathBuf>, source: zip::result::ZipError) -> Self {
        Self::ZipWithPath {
            path: path.into(),
            source,
        }
    }

    pub fn archive_format(path: impl Into<PathBuf>) -> Self {
        Self::ArchiveFormat { path: path.into() }
    }
}
