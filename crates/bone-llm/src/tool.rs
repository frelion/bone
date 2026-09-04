use std::{collections::HashSet, fmt, sync::Arc};

use rig_core::message::{ToolCall as RigToolCall, ToolResultContent, UserContent};
use serde_json::Value;

use crate::{Error, model::RequestOrigin};

/// Every correlation namespace seen in committed assistant history.
///
/// Keeping this inside `bone-llm` lets callers treat a [`ToolCall`] as one
/// opaque capability while the model boundary still rejects duplicate wire
/// handles before any tool can be executed.
#[derive(Default)]
pub(crate) struct ToolCallIdentities {
    ids: HashSet<String>,
    provider_call_ids: HashSet<String>,
    provider_item_ids: HashSet<String>,
}

impl ToolCallIdentities {
    pub(crate) fn insert(&mut self, call: &RigToolCall) -> Result<(), Error> {
        if !self.ids.insert(call.id.as_str().to_owned()) {
            return Err(Error::protocol(
                "model response reused a tool-call identifier",
            ));
        }

        if let Some(provider) = &call.provider {
            if provider.call_id.is_empty() {
                return Err(Error::protocol(
                    "model response contained an empty provider tool-call identifier",
                ));
            }
            if !self.provider_call_ids.insert(provider.call_id.clone()) {
                return Err(Error::protocol(
                    "model response reused a provider tool-call identifier",
                ));
            }
            if let Some(item_id) = &provider.item_id {
                if item_id.is_empty() {
                    return Err(Error::protocol(
                        "model response contained an empty provider tool item identifier",
                    ));
                }
                if !self.provider_item_ids.insert(item_id.clone()) {
                    return Err(Error::protocol(
                        "model response reused a provider tool item identifier",
                    ));
                }
            }
        }

        if call.function.name.trim().is_empty() {
            return Err(Error::protocol(
                "model response contained a tool call with an empty name",
            ));
        }
        Ok(())
    }
}

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

/// An explicit constraint on how the model may select supplied tools.
///
/// Omit [`crate::Request::tool_choice`] for ordinary automatic selection.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolChoice {
    None,
    Required,
    Specific(Vec<String>),
}

impl ToolChoice {
    pub(crate) fn into_rig(self) -> rig_core::message::ToolChoice {
        match self {
            Self::None => rig_core::message::ToolChoice::None,
            Self::Required => rig_core::message::ToolChoice::Required,
            Self::Specific(function_names) => {
                rig_core::message::ToolChoice::Specific { function_names }
            }
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

    pub(crate) fn result_content(&self, output: ToolOutput) -> UserContent {
        UserContent::tool_result_for(
            self.inner.id.clone(),
            self.inner.provider.clone(),
            self.inner.function.name.clone(),
            output.content,
        )
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
