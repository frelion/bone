use std::{fmt, sync::Arc};

use rig_core::message::{AssistantContent, Message, Text, UserContent};

use crate::{ToolCall, ToolOutput, model::RequestOrigin};

/// Who supplied an external input item.
///
/// This is attribution, not authority. Named participants are encoded as
/// ordinary external input and can never become model instructions.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputSource {
    User,
    Named(String),
}

/// One ordered item supplied to a model request.
#[derive(Clone, PartialEq)]
pub struct InputItem {
    pub(crate) kind: InputItemKind,
}

#[derive(Clone, PartialEq)]
pub(crate) enum InputItemKind {
    External {
        source: InputSource,
        text: String,
    },
    AssistantExample(String),
    AssistantReplay {
        origin: Arc<RequestOrigin>,
        message: Message,
    },
    ToolResult {
        call: ToolCall,
        output: ToolOutput,
    },
}

impl fmt::Debug for InputItem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut item = formatter.debug_struct("InputItem");
        match &self.kind {
            InputItemKind::External { source, .. } => item
                .field("kind", &"external")
                .field("source", source)
                .field("content", &"<redacted>"),
            InputItemKind::AssistantExample(_) => item
                .field("kind", &"assistant-example")
                .field("content", &"<redacted>"),
            InputItemKind::AssistantReplay { .. } => item
                .field("kind", &"assistant-replay")
                .field("content", &"<opaque>"),
            InputItemKind::ToolResult { call, .. } => item
                .field("kind", &"tool-result")
                .field("call_id", &call.id())
                .field("content", &"<redacted>"),
        };
        item.finish()
    }
}

impl InputItem {
    /// External text attributed to a human or another named participant.
    pub fn external(source: InputSource, text: impl Into<String>) -> Self {
        Self {
            kind: InputItemKind::External {
                source,
                text: text.into(),
            },
        }
    }

    /// Assistant text used as an explicit few-shot example.
    pub fn assistant_example(text: impl Into<String>) -> Self {
        Self {
            kind: InputItemKind::AssistantExample(text.into()),
        }
    }

    /// Return the result of one exact tool call to the model that issued it.
    pub fn tool_result(call: &ToolCall, output: ToolOutput) -> Self {
        Self {
            kind: InputItemKind::ToolResult {
                call: call.clone(),
                output,
            },
        }
    }

    pub(crate) fn assistant_replay(origin: Arc<RequestOrigin>, message: Message) -> Self {
        Self {
            kind: InputItemKind::AssistantReplay { origin, message },
        }
    }

    pub(crate) fn external_message(source: InputSource, text: String) -> Message {
        let text = match source {
            InputSource::User => text,
            InputSource::Named(name) => {
                let encoded =
                    serde_json::to_string(&name).expect("serializing a string as JSON cannot fail");
                format!("<bone_external source={encoded}>\n{text}\n</bone_external>")
            }
        };
        Message::User {
            content: vec![UserContent::Text(Text::new(text))],
        }
    }

    pub(crate) fn assistant_example_message(text: String) -> Message {
        Message::Assistant {
            id: None,
            content: vec![AssistantContent::text(text)],
        }
    }
}
