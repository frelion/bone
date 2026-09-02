use std::{io, path::PathBuf};

use thiserror::Error;

/// Failures produced while registering, reading, validating, or writing BONE
/// configuration.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("configuration path must be absolute")]
    RelativePath,
    #[error("configuration path has no parent directory")]
    MissingParent,
    #[error("invalid configuration section key: {0}")]
    InvalidSectionKey(String),
    #[error("configuration section is already registered: {0}")]
    DuplicateSection(String),
    #[error("configuration section type does not match its registration: {0}")]
    SectionTypeMismatch(String),
    #[error("unknown configuration section: {0}")]
    UnknownSection(String),
    #[error("invalid configuration section `{section}`: {message}")]
    InvalidSection { section: String, message: String },
    #[error("configuration must be a JSON object")]
    InvalidDocument,
    #[error("configuration exceeds the {maximum_bytes}-byte limit")]
    DocumentTooLarge { maximum_bytes: usize },
    #[error("configuration changed since it was read")]
    RevisionConflict,
    #[error("configuration storage is busy")]
    Busy,
    #[error("invalid configuration revision; expected 64 lowercase hexadecimal characters")]
    InvalidRevision,
    #[error("unsafe configuration storage at {path}: {reason}")]
    UnsafeStorage { path: PathBuf, reason: String },
    #[error("failed to {operation} configuration at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to encode configuration: {0}")]
    Encode(#[from] serde_json::Error),
}

impl ConfigError {
    pub(crate) fn io(operation: &'static str, path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            operation,
            path: path.into(),
            source,
        }
    }
}
