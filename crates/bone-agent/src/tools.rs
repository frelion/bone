use std::{
    collections::BTreeMap, error::Error as StdError, fmt, future::Future, pin::Pin, sync::Arc,
};

use bone_llm::{ToolDefinition, ToolOutput};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::AgentConfigError;

pub(crate) const START_ACTION_TOOL: &str = "start_action";

/// A typed operation that an [`crate::Agent`] may give to an action.
///
/// `call` must not block its executor thread. Its future may be dropped on
/// cancellation or timeout, so cleanup must not depend on the future running
/// to completion.
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

    /// Convert an implementation error into the agent's stable failure type.
    ///
    /// The default retains the original error for operators while exposing
    /// only a generic message to the model.
    fn map_error(&self, error: Self::Error) -> ToolFailure {
        let message = error.to_string();
        ToolFailure::new(
            ToolFailureKind::Other,
            message,
            ToolOutput::text("tool execution failed"),
        )
        .with_source(error)
    }
}

/// Stable classification of a failed tool execution.
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
    /// Create a failure. `message` is for operators; `model_output` is the
    /// only failure detail sent back to the model.
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

    /// Preserve the implementation error for diagnostics without exposing it
    /// to the model.
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

pub(crate) enum ToolOutcome {
    Success(ToolOutput),
    Failure(ToolFailure),
    Skipped(ToolOutput),
}

impl ToolOutcome {
    pub(crate) fn model_output(&self) -> &ToolOutput {
        match self {
            Self::Success(output) | Self::Skipped(output) => output,
            Self::Failure(failure) => failure.model_output(),
        }
    }

    pub(crate) fn success(output: ToolOutput) -> Self {
        Self::Success(output)
    }

    pub(crate) fn failed(failure: ToolFailure) -> Self {
        Self::Failure(failure)
    }

    pub(crate) fn skipped(reason: impl Into<String>) -> Self {
        Self::Skipped(ToolOutput::text(reason))
    }
}

type ToolFuture = Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'static>>;

struct DynamicTool {
    definition: ToolDefinition,
    call: Box<dyn Fn(Value) -> ToolFuture + Send + Sync>,
}

impl DynamicTool {
    fn erase<T>(tool: T) -> Self
    where
        T: Tool + 'static,
    {
        let definition = tool.definition();
        let name = definition.name().to_owned();
        let tool = Arc::new(tool);
        let call = Box::new(move |arguments: Value| {
            let tool = Arc::clone(&tool);
            let name = name.clone();
            let future: ToolFuture = Box::pin(async move {
                let arguments = match serde_json::from_value::<T::Args>(arguments) {
                    Ok(arguments) => arguments,
                    Err(error) => {
                        let feedback =
                            format!("arguments for tool `{name}` did not match its schema");
                        let failure = ToolFailure::new(
                            ToolFailureKind::InvalidArguments,
                            feedback.clone(),
                            ToolOutput::text(feedback),
                        )
                        .with_source(error);
                        return ToolOutcome::failed(failure);
                    }
                };
                let output = match tool.call(arguments).await {
                    Ok(output) => output,
                    Err(error) => return ToolOutcome::failed(tool.map_error(error)),
                };
                match serde_json::to_value(output) {
                    Ok(Value::String(text)) => ToolOutcome::success(ToolOutput::text(text)),
                    Ok(value) => ToolOutcome::success(ToolOutput::json(value)),
                    Err(error) => ToolOutcome::failed(
                        ToolFailure::new(
                            ToolFailureKind::Other,
                            format!("tool `{name}` returned a value that could not be serialized"),
                            ToolOutput::text("tool output could not be serialized"),
                        )
                        .with_source(error),
                    ),
                }
            });
            future
        });
        Self { definition, call }
    }
}

#[derive(Default)]
pub(crate) struct Tools {
    entries: BTreeMap<String, DynamicTool>,
}

impl Tools {
    pub(crate) fn register<T>(&mut self, tool: T) -> Result<(), AgentConfigError>
    where
        T: Tool + 'static,
    {
        let tool = DynamicTool::erase(tool);
        let name = tool.definition.name().to_owned();
        if name == START_ACTION_TOOL {
            return Err(AgentConfigError::ReservedTool);
        }
        if self.entries.contains_key(&name) {
            return Err(AgentConfigError::DuplicateTool(name));
        }
        self.entries.insert(name, tool);
        Ok(())
    }

    pub(crate) fn definitions(&self) -> Vec<ToolDefinition> {
        self.entries
            .values()
            .map(|tool| tool.definition.clone())
            .collect()
    }

    pub(crate) fn execute(&self, name: &str, arguments: Value) -> Option<ToolFuture> {
        self.entries.get(name).map(|tool| (tool.call)(arguments))
    }
}

pub(crate) fn missing_tool(name: &str) -> ToolOutcome {
    let feedback = format!("tool `{name}` is not registered");
    ToolOutcome::failed(ToolFailure::new(
        ToolFailureKind::NotFound,
        feedback.clone(),
        ToolOutput::text(feedback),
    ))
}
