use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures_util::Stream;
use rig_core::streaming::{
    StreamedAssistantContent, StreamingCompletionResponse, ToolCallDeltaContent as RigToolCallDelta,
};

use crate::{Error, Response, model::RequestOrigin, tool::ToolCallIdentities};

/// One partial tool-call field emitted while a call is assembling.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolCallDelta {
    Name(String),
    Arguments(String),
}

/// One event from a streaming model call.
///
/// Every fully consumed stream ends in exactly one terminal item: either
/// `Completed(Response)` or `Err(Error)`.
#[non_exhaustive]
#[derive(Clone, Debug)]
pub enum StreamEvent {
    TextDelta(String),
    ToolCallDelta { id: String, delta: ToolCallDelta },
    Completed(Response),
}

/// A provider-independent model response stream.
pub struct ResponseStream {
    inner: Option<StreamingCompletionResponse>,
    origin: Arc<RequestOrigin>,
    previous_tool_calls: ToolCallIdentities,
    finished: bool,
}

impl ResponseStream {
    pub(crate) fn new(
        inner: StreamingCompletionResponse,
        origin: Arc<RequestOrigin>,
        previous_tool_calls: ToolCallIdentities,
    ) -> Self {
        Self {
            inner: Some(inner),
            origin,
            previous_tool_calls,
            finished: false,
        }
    }
}

impl Stream for ResponseStream {
    type Item = Result<StreamEvent, Error>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished {
            return Poll::Ready(None);
        }

        loop {
            let inner = this
                .inner
                .as_mut()
                .expect("an unfinished response stream owns its inner stream");
            match Pin::new(inner).poll_next(context) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Err(error))) => {
                    this.finished = true;
                    this.inner.take();
                    return Poll::Ready(Some(Err(Error::from_rig(error))));
                }
                Poll::Ready(Some(Ok(event))) => match event {
                    StreamedAssistantContent::Text(text) => {
                        return Poll::Ready(Some(Ok(StreamEvent::TextDelta(text.text))));
                    }
                    StreamedAssistantContent::ToolCall {
                        tool_call: _,
                        internal_call_id: _,
                    } => {}
                    StreamedAssistantContent::ToolCallDelta {
                        internal_call_id,
                        content,
                    } => {
                        let delta = match content {
                            RigToolCallDelta::Name(name) => ToolCallDelta::Name(name),
                            RigToolCallDelta::Delta(arguments) => {
                                ToolCallDelta::Arguments(arguments)
                            }
                        };
                        return Poll::Ready(Some(Ok(StreamEvent::ToolCallDelta {
                            id: internal_call_id,
                            delta,
                        })));
                    }
                    // Rig finishes aggregation only on the poll *after* Final.
                    StreamedAssistantContent::Final(_)
                    | StreamedAssistantContent::Reasoning { .. }
                    | StreamedAssistantContent::ReasoningDelta { .. }
                    | StreamedAssistantContent::Unknown(_) => {}
                },
                Poll::Ready(None) => {
                    this.finished = true;
                    let inner = this
                        .inner
                        .take()
                        .expect("a completed response stream owns its inner stream");
                    if inner.response.is_none() {
                        return Poll::Ready(Some(Err(Error::incomplete_stream())));
                    }
                    let raw = inner
                        .response
                        .as_ref()
                        .map(|terminal| terminal.raw.clone())
                        .unwrap_or_default();
                    let response =
                        rig_core::completion::CompletionResponse::from(inner).with_raw(raw);
                    let previous_tool_calls = std::mem::take(&mut this.previous_tool_calls);
                    return Poll::Ready(Some(
                        Response::from_rig(Arc::clone(&this.origin), response, previous_tool_calls)
                            .map(StreamEvent::Completed),
                    ));
                }
            }
        }
    }
}
