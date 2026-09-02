use std::io;

use rig_core::tool::ToolExecutionError;
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

    pub(crate) fn into_execution_error(self) -> ToolExecutionError {
        let message = self.to_string();
        match &self {
            Self::InvalidArgs(_)
            | Self::InvalidGlob(_)
            | Self::InvalidRegex(_)
            | Self::Patch(_) => ToolExecutionError::invalid_args(message).with_source(self),
            Self::NotFound { .. } => ToolExecutionError::not_found(message).with_source(self),
            Self::OutsideWorkspace { .. } | Self::PermissionDenied { .. } => {
                ToolExecutionError::permission_denied(message).with_source(self)
            }
            Self::Io { source, .. } if source.kind() == io::ErrorKind::PermissionDenied => {
                ToolExecutionError::permission_denied(message).with_source(self)
            }
            Self::Io { source, .. } if source.kind() == io::ErrorKind::NotFound => {
                ToolExecutionError::not_found(message).with_source(self)
            }
            Self::Io {
                operation, path, ..
            } => ToolExecutionError::other(message)
                .with_model_feedback(format!("{operation} failed for {path}"))
                .with_source(self),
            Self::Task(_) => ToolExecutionError::other(message)
                .with_model_feedback("background tool task failed")
                .with_source(self),
            Self::Spawn { shell, .. } => ToolExecutionError::other(message)
                .with_model_feedback(format!("failed to start shell `{shell}`"))
                .with_source(self),
        }
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
        .into_execution_error();

        assert!(error.message().contains("/private/workspace"));
        assert_eq!(
            error.model_feedback(),
            Some("read file failed for src/lib.rs")
        );
    }
}
