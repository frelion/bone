use std::{io, path::PathBuf};

use rusqlite::{Error as SqliteError, ErrorCode};
use thiserror::Error;

use crate::Revision;

/// Failures from BONE's local durable store.
///
/// This error deliberately does not contain stored document payloads. BONE
/// state can contain user conversation text, so callers should surface a
/// concise presentation error instead of logging arbitrary values.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("BONE store root must be absolute: {path}")]
    RelativeRoot { path: PathBuf },
    #[error("could not determine a default local BONE store root")]
    MissingDefaultRoot,
    #[error("unsafe BONE storage at {path}: {reason}")]
    UnsafeStorage { path: PathBuf, reason: &'static str },
    #[error("BONE storage is busy")]
    Busy,
    #[error(
        "stored document changed since it was read (expected revision {expected}, actual {actual})"
    )]
    RevisionConflict {
        expected: Revision,
        actual: Revision,
    },
    #[error("stored document revision cannot be incremented further")]
    RevisionExhausted,
    #[error("persistent payload exceeds the {maximum_bytes}-byte limit")]
    PayloadTooLarge { maximum_bytes: usize },
    #[error("internal BONE storage identifier is invalid")]
    InvalidIdentifier,
    #[error("BONE storage data is corrupt: {message}")]
    Corrupt { message: &'static str },
    #[error("system time cannot be represented as Unix milliseconds")]
    Clock,
    #[error("BONE store schema version {found} is unsupported")]
    UnsupportedSchema { found: i64 },
    #[error("failed to encode BONE persistent data")]
    Encode(#[source] serde_json::Error),
    #[error("failed to decode BONE persistent data")]
    Decode(#[source] serde_json::Error),
    #[error("failed to {operation} BONE storage at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("SQLite storage failed while {operation}: {source}")]
    Sqlite {
        operation: &'static str,
        #[source]
        source: SqliteError,
    },
}

impl StoreError {
    pub(crate) fn io(operation: &'static str, path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            operation,
            path: path.into(),
            source,
        }
    }

    pub(crate) fn sqlite(operation: &'static str, source: SqliteError) -> Self {
        match &source {
            SqliteError::SqliteFailure(error, _)
                if matches!(
                    error.code,
                    ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
                ) =>
            {
                Self::Busy
            }
            SqliteError::SqliteFailure(error, _)
                if matches!(
                    error.code,
                    ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase
                ) =>
            {
                Self::Corrupt {
                    message: "SQLite database is corrupt or not a database",
                }
            }
            _ => Self::Sqlite { operation, source },
        }
    }
}

/// Redacted failures from a provider-managed OAuth cache.
///
/// The cache is owned by Rig and can contain refresh tokens. This boundary
/// intentionally hides paths, file metadata, and all OAuth payloads.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ProviderAuthError {
    #[error("provider credential cache is in use by another BONE process")]
    Busy,
    #[error("provider credential cache is unavailable or unsafe")]
    Unavailable,
}

impl From<StoreError> for ProviderAuthError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Busy => Self::Busy,
            _ => Self::Unavailable,
        }
    }
}
