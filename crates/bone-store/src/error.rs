use std::{io, path::PathBuf};

use rusqlite::{Error as SqliteError, ErrorCode};
use thiserror::Error;

use crate::Revision;

/// Failures from the local durable store.
///
/// This error deliberately does not contain stored document payloads. Callers
/// should surface a concise presentation error instead of logging arbitrary
/// values.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("store root must be absolute: {path}")]
    RelativeRoot { path: PathBuf },
    #[error("unsafe local storage at {path}: {reason}")]
    UnsafeStorage { path: PathBuf, reason: &'static str },
    #[error("local storage is busy")]
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
    #[error("a document or journal belongs to a different store")]
    WrongStore,
    #[error("persistent data is corrupt: {message}")]
    Corrupt { message: &'static str },
    #[error("system time cannot be represented as Unix milliseconds")]
    Clock,
    #[error("store schema version {found} is unsupported")]
    UnsupportedSchema { found: i64 },
    #[error("failed to encode persistent data")]
    Encode(#[source] serde_json::Error),
    #[error("failed to decode persistent data")]
    Decode(#[source] serde_json::Error),
    #[error("failed to {operation} local storage at {path}: {source}")]
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
