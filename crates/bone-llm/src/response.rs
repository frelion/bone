use std::{fmt, sync::Arc};

use rig_core::{
    completion::{AssistantContent, CompletionResponse, FinishReason as RigFinishReason},
    message::{Message, ReasoningContent},
};

use crate::{
    Error, InputItem, OutputItem, Protocol, ToolCall, model::RequestOrigin,
    tool::ToolCallIdentities,
};

/// Why a model stopped generating.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Other(String),
}

impl FinishReason {
    pub fn truncated_output(&self) -> bool {
        matches!(self, Self::Length | Self::ContentFilter)
    }
}

impl FinishReason {
    fn from_rig(value: RigFinishReason) -> Self {
        match value {
            RigFinishReason::Stop => Self::Stop,
            RigFinishReason::Length => Self::Length,
            RigFinishReason::ToolCalls => Self::ToolCalls,
            RigFinishReason::ContentFilter => Self::ContentFilter,
            RigFinishReason::Other(value) => Self::Other(value),
        }
    }
}

/// Token accounting reported by the provider.
///
/// All zeroes means the provider did not report usage; it is not proof that no
/// tokens were consumed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub tool_use_prompt_tokens: u64,
    pub reasoning_tokens: u64,
}

impl Usage {
    pub fn was_reported(&self) -> bool {
        *self != Self::default()
    }
}

impl Usage {
    fn from_rig(value: rig_core::completion::Usage) -> Self {
        Self {
            input_tokens: value.input_tokens,
            output_tokens: value.output_tokens,
            total_tokens: value.total_tokens,
            cached_input_tokens: value.cached_input_tokens,
            cache_creation_input_tokens: value.cache_creation_input_tokens,
            tool_use_prompt_tokens: value.tool_use_prompt_tokens,
            reasoning_tokens: value.reasoning_tokens,
        }
    }
}

/// Where one response came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResponseOrigin {
    request: Arc<RequestOrigin>,
    provider: String,
    reported_model_id: Option<String>,
}

impl ResponseOrigin {
    pub fn endpoint_id(&self) -> &str {
        &self.request.endpoint_id
    }

    pub fn protocol(&self) -> Protocol {
        self.request.protocol
    }

    pub fn requested_model_id(&self) -> &str {
        &self.request.model_id
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    pub fn reported_model_id(&self) -> Option<&str> {
        self.reported_model_id.as_deref()
    }
}

#[derive(Clone, Debug)]
struct ResponseData {
    origin: ResponseOrigin,
    items: Vec<OutputItem>,
    usage: Usage,
    finish_reason: Option<FinishReason>,
    message_id: Option<String>,
    response_id: Option<String>,
    provider_request_id: Option<String>,
    replay: Option<Message>,
}

/// One completed model response.
///
/// Cloning is cheap. Full normalized assistant state is retained privately so
/// [`Response::into_item`] can replay it without callers handling reasoning
/// signatures or provider correlation identifiers.
#[derive(Clone)]
pub struct Response {
    inner: Arc<ResponseData>,
}

impl fmt::Debug for Response {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let item_kinds = self
            .items()
            .iter()
            .map(|item| match item {
                OutputItem::Text(_) => "text",
                OutputItem::ToolCall(_) => "tool-call",
                OutputItem::ReasoningSummary(_) => "reasoning-summary",
            })
            .collect::<Vec<_>>();
        formatter
            .debug_struct("Response")
            .field("origin", &self.inner.origin)
            .field("item_kinds", &item_kinds)
            .field("usage", &self.inner.usage)
            .field("finish_reason", &self.inner.finish_reason)
            .field("message_id", &self.inner.message_id)
            .field("response_id", &self.inner.response_id)
            .field("provider_request_id", &self.inner.provider_request_id)
            .finish()
    }
}

impl Response {
    pub(crate) fn from_rig(
        origin: Arc<RequestOrigin>,
        response: CompletionResponse,
        mut previous_tool_calls: ToolCallIdentities,
    ) -> Result<Self, Error> {
        let mut items = Vec::new();
        for item in &response.choice {
            match item {
                AssistantContent::Text(text) => items.push(OutputItem::Text(text.text.clone())),
                AssistantContent::ToolCall(call) => {
                    previous_tool_calls.insert(call)?;
                    items.push(OutputItem::ToolCall(ToolCall::from_rig(
                        Arc::clone(&origin),
                        call.clone(),
                    )));
                }
                AssistantContent::Reasoning(reasoning) => {
                    items.extend(
                        reasoning
                            .content
                            .iter()
                            .filter_map(|content| match content {
                                ReasoningContent::Summary(summary) => {
                                    Some(OutputItem::ReasoningSummary(summary.clone()))
                                }
                                ReasoningContent::Text { .. }
                                | ReasoningContent::Encrypted(_)
                                | ReasoningContent::Redacted { .. } => None,
                            }),
                    );
                }
                AssistantContent::Image(_) => {
                    return Err(Error::protocol(
                        "model returned image output that bone-llm cannot represent",
                    ));
                }
            }
        }

        let replay = (!response.choice.is_empty()).then(|| Message::Assistant {
            id: response.message_id.clone(),
            content: response.choice.clone(),
        });
        let response_origin = ResponseOrigin {
            request: origin,
            provider: response.provider.clone(),
            reported_model_id: response.model.clone(),
        };
        let data = ResponseData {
            origin: response_origin,
            items,
            usage: Usage::from_rig(response.usage),
            finish_reason: response.finish_reason().map(FinishReason::from_rig),
            message_id: response.message_id,
            response_id: response.response_id,
            provider_request_id: response.provider_request_id,
            replay,
        };
        Ok(Self {
            inner: Arc::new(data),
        })
    }

    pub fn origin(&self) -> &ResponseOrigin {
        &self.inner.origin
    }

    pub fn items(&self) -> &[OutputItem] {
        &self.inner.items
    }

    pub fn text(&self) -> Option<String> {
        let mut text = String::new();
        let mut found = false;
        for item in self.items() {
            if let OutputItem::Text(part) = item {
                found = true;
                text.push_str(part);
            }
        }
        (found && !text.trim().is_empty()).then_some(text)
    }

    pub fn tool_calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.items().iter().filter_map(|item| match item {
            OutputItem::ToolCall(call) => Some(call),
            _ => None,
        })
    }

    pub fn usage(&self) -> Usage {
        self.inner.usage
    }

    pub fn finish_reason(&self) -> Option<&FinishReason> {
        self.inner.finish_reason.as_ref()
    }

    pub fn message_id(&self) -> Option<&str> {
        self.inner.message_id.as_deref()
    }

    pub fn response_id(&self) -> Option<&str> {
        self.inner.response_id.as_deref()
    }

    pub fn provider_request_id(&self) -> Option<&str> {
        self.inner.provider_request_id.as_deref()
    }

    /// Convert this exact response into its canonical next-request item.
    ///
    /// `None` means the provider legally returned no assistant content;
    /// fabricating an empty history item would make the next request invalid.
    pub fn into_item(self) -> Option<InputItem> {
        self.inner.replay.clone().map(|message| {
            InputItem::assistant_replay(Arc::clone(&self.inner.origin.request), message)
        })
    }
}
