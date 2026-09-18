use std::{fmt, sync::Arc};

use rig_core::message::{ToolCall as RigToolCall, ToolResultContent};
use serde_json::Value;

use crate::llm::model::RequestOrigin;

/// A function the model may ask the caller to execute.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolDefinition {
    name: String,
    description: String,
    parameters: Value,
}

impl ToolDefinition {
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn parameters(&self) -> &Value {
        &self.parameters
    }

    pub(crate) fn into_rig(self) -> rig_core::completion::ToolDefinition {
        rig_core::completion::ToolDefinition {
            name: self.name,
            description: self.description,
            parameters: self.parameters,
        }
    }
}

/// One complete tool invocation requested by a model.
#[derive(Clone, PartialEq)]
pub struct ToolCall {
    pub(crate) origin: Arc<RequestOrigin>,
    pub(crate) inner: RigToolCall,
}

impl ToolCall {
    pub(crate) fn from_rig(origin: Arc<RequestOrigin>, inner: RigToolCall) -> Self {
        Self { origin, inner }
    }

    /// Stable correlation identifier for this invocation.
    pub fn id(&self) -> &str {
        self.inner.id.as_str()
    }

    /// Provider-facing function name.
    pub fn name(&self) -> &str {
        &self.inner.function.name
    }

    /// Parsed JSON arguments produced by the model.
    pub fn arguments(&self) -> &Value {
        &self.inner.function.arguments
    }
}

impl fmt::Debug for ToolCall {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolCall")
            .field("id", &self.id())
            .field("name", &self.name())
            .field("arguments", &"<redacted>")
            .finish()
    }
}

/// Canonical model-visible output from a tool execution.
#[derive(Clone, PartialEq)]
pub struct ToolOutput {
    pub(crate) content: Vec<ToolResultContent>,
}

impl ToolOutput {
    /// Literal text output.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ToolResultContent::text(text)],
        }
    }

    /// Structured JSON output. A JSON string remains JSON, not plain text.
    pub fn json(value: Value) -> Self {
        Self {
            content: vec![ToolResultContent::json(value)],
        }
    }

    /// Borrow the literal text when this output contains text.
    pub fn as_text(&self) -> Option<&str> {
        self.content.first().and_then(ToolResultContent::as_text)
    }

    /// Borrow the structured value when this output contains JSON.
    pub fn as_json(&self) -> Option<&Value> {
        self.content.first().and_then(ToolResultContent::as_json)
    }
}

impl fmt::Debug for ToolOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolOutput")
            .field("content_count", &self.content.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::ToolOutput;

    #[test]
    fn tool_output_preserves_text_and_json_kinds() {
        let text = ToolOutput::text("hello");
        assert_eq!(text.as_text(), Some("hello"));
        assert_eq!(text.as_json(), None);

        let value = json!({ "answer": 42 });
        let structured = ToolOutput::json(value.clone());
        assert_eq!(structured.as_text(), None);
        assert_eq!(structured.as_json(), Some(&value));
    }
}
