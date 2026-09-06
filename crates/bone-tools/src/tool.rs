use std::{error::Error as StdError, fmt, future::Future};

use bone_llm::{ToolDefinition, ToolOutput};
use serde::{Serialize, de::DeserializeOwned};

/// A native tool with typed arguments, output, and implementation errors.
///
/// `call` must not block its executor thread. Hosts decide how to schedule,
/// cancel, and expose calls to an agent; tools do not manage an agent loop.
/// A host may drop the future, so cleanup must not depend on polling it again.
pub trait Tool: Send + Sync {
    type Args: DeserializeOwned + Send + 'static;
    type Output: Serialize + Send + 'static;
    type Error: StdError + Send + Sync + 'static;

    /// The name, description, and JSON schema shown to the model.
    fn definition(&self) -> ToolDefinition;

    /// Execute one validated call.
    fn call(
        &self,
        arguments: Self::Args,
    ) -> impl Future<Output = Result<Self::Output, Self::Error>> + Send;

    /// Keep implementation diagnostics separate from model-facing feedback.
    fn map_error(&self, error: Self::Error) -> ToolFailure {
        ToolFailure::new(
            ToolFailureKind::Other,
            error.to_string(),
            ToolOutput::text("tool execution failed"),
        )
        .with_source(error)
    }
}

/// Stable classification of a failed native tool call.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolFailureKind {
    InvalidArguments,
    Timeout,
    NotFound,
    PermissionDenied,
    Other,
}

/// A tool failure with separate operator and model-facing information.
pub struct ToolFailure {
    kind: ToolFailureKind,
    message: String,
    model_output: ToolOutput,
    source: Option<Box<dyn StdError + Send + Sync>>,
}

impl ToolFailure {
    pub fn new(
        kind: ToolFailureKind,
        message: impl Into<String>,
        model_output: ToolOutput,
    ) -> Self {
        Self {
            kind,
            message: message.into(),
            model_output,
            source: None,
        }
    }

    pub fn with_source(mut self, source: impl StdError + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    pub fn kind(&self) -> ToolFailureKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn model_output(&self) -> &ToolOutput {
        &self.model_output
    }
}

impl fmt::Display for ToolFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl fmt::Debug for ToolFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolFailure")
            .field("kind", &self.kind)
            .field("has_source", &self.source.is_some())
            .finish()
    }
}

impl StdError for ToolFailure {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn StdError + 'static))
    }
}
