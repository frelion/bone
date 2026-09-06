use std::io;

use crate::{ToolFailure, ToolFailureKind};
use bone_llm::ToolOutput;
use thiserror::Error;

/// Concrete failures produced by built-in tools.
#[derive(Debug, Error)]
pub enum ToolError {
    #[error("invalid arguments: {0}")]
    InvalidArgs(String),
    #[error("path not found: {path}")]
    NotFound { path: String },
    #[error("path is outside the workspace: {path}")]
    OutsideWorkspace { path: String },
    #[error("permission denied: {path}")]
    PermissionDenied { path: String },
    #[error("{operation} failed for {path}: {source}")]
    Io {
        operation: &'static str,
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("invalid glob pattern: {0}")]
    InvalidGlob(String),
    #[error("invalid search pattern: {0}")]
    InvalidRegex(String),
    #[error("background tool task failed: {0}")]
    Task(String),
    #[error("failed to start shell `{shell}`: {source}")]
    Spawn {
        shell: String,
        #[source]
        source: io::Error,
    },
    #[error("patch failed: {0}")]
    Patch(String),
}

impl ToolError {
    pub(crate) fn io_display(
        operation: &'static str,
        path: impl Into<String>,
        source: io::Error,
    ) -> Self {
        let path = path.into();
        if source.kind() == io::ErrorKind::NotFound {
            return Self::NotFound { path };
        }
        if source.kind() == io::ErrorKind::PermissionDenied {
            return Self::PermissionDenied { path };
        }
        Self::Io {
            operation,
            path,
            source,
        }
    }

    pub(crate) fn into_tool_failure(self) -> ToolFailure {
        let message = self.to_string();
        let (kind, model_output) = match &self {
            Self::InvalidArgs(_)
            | Self::InvalidGlob(_)
            | Self::InvalidRegex(_)
            | Self::Patch(_) => (ToolFailureKind::InvalidArguments, message.clone()),
            Self::NotFound { .. } => (ToolFailureKind::NotFound, message.clone()),
            Self::OutsideWorkspace { .. } | Self::PermissionDenied { .. } => {
                (ToolFailureKind::PermissionDenied, message.clone())
            }
            Self::Io { source, .. } if source.kind() == io::ErrorKind::PermissionDenied => {
                (ToolFailureKind::PermissionDenied, message.clone())
            }
            Self::Io { source, .. } if source.kind() == io::ErrorKind::NotFound => {
                (ToolFailureKind::NotFound, message.clone())
            }
            Self::Io {
                operation, path, ..
            } => (
                ToolFailureKind::Other,
                format!("{operation} failed for {path}"),
            ),
            Self::Task(_) => (
                ToolFailureKind::Other,
                "background tool task failed".to_owned(),
            ),
            Self::Spawn { shell, .. } => (
                ToolFailureKind::Other,
                format!("failed to start shell `{shell}`"),
            ),
        };
        ToolFailure::new(kind, message, ToolOutput::text(model_output)).with_source(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_sources_remain_operator_only() {
        let error = ToolError::Io {
            operation: "read file",
            path: "src/lib.rs".to_owned(),
            source: io::Error::other("host detail /private/workspace"),
        }
        .into_tool_failure();

        assert!(error.message().contains("/private/workspace"));
        assert_eq!(
            error.model_output(),
            &ToolOutput::text("read file failed for src/lib.rs")
        );
        assert!(std::error::Error::source(&error).is_some());
    }
}
