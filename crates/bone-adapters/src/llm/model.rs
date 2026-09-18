use std::{fmt, future::Future, pin::Pin, sync::Arc};

use rig_core::{
    completion::{CompletionError, CompletionModel, CompletionRequest},
    streaming::StreamingCompletionResponse,
};

use crate::llm::{Error, Protocol, Request, ResponseStream};

type StreamFuture<'a> =
    Pin<Box<dyn Future<Output = Result<StreamingCompletionResponse, CompletionError>> + Send + 'a>>;

/// A provider model behind BONE's one call shape.
///
/// Only `stream` is erased, because only `stream` is ever called.
trait ErasedModel: Send + Sync {
    fn stream(&self, request: CompletionRequest) -> StreamFuture<'_>;
}

impl<M> ErasedModel for M
where
    M: CompletionModel + Send + Sync + 'static,
{
    fn stream(&self, request: CompletionRequest) -> StreamFuture<'_> {
        Box::pin(CompletionModel::stream(self, request))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RequestOrigin {
    pub(crate) endpoint_id: Arc<str>,
    pub(crate) protocol: Protocol,
    pub(crate) model_id: Arc<str>,
}

/// A selected model behind one configured endpoint.
#[derive(Clone)]
pub struct Model {
    origin: Arc<RequestOrigin>,
    inner: Arc<dyn ErasedModel>,
}

impl fmt::Debug for Model {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Model")
            .field("endpoint_id", &self.origin.endpoint_id)
            .field("protocol", &self.origin.protocol)
            .field("id", &self.origin.model_id)
            .finish_non_exhaustive()
    }
}

impl Model {
    pub(crate) fn new(
        endpoint_id: Arc<str>,
        protocol: Protocol,
        id: Arc<str>,
        inner: impl CompletionModel + Send + Sync + 'static,
    ) -> Self {
        Self {
            origin: Arc::new(RequestOrigin {
                endpoint_id,
                protocol,
                model_id: id,
            }),
            inner: Arc::new(inner),
        }
    }

    /// The application-defined endpoint identity.
    pub fn endpoint_id(&self) -> &str {
        &self.origin.endpoint_id
    }

    /// The wire protocol used by this model.
    pub fn protocol(&self) -> Protocol {
        self.origin.protocol
    }

    /// The configured model identifier.
    pub fn id(&self) -> &str {
        &self.origin.model_id
    }

    /// Open one streaming request.
    ///
    /// This is the only model-call entry point. A caller that only wants the
    /// final result consumes the stream to its terminal event.
    pub async fn stream(&self, request: Request) -> Result<ResponseStream, Error> {
        let request = request.into_rig(&self.origin)?;
        let stream = self.inner.stream(request).await.map_err(Error::from_rig)?;
        Ok(ResponseStream::new(stream, Arc::clone(&self.origin)))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use futures_util::{StreamExt, stream};
    use rig_core::{
        completion::{
            AssistantContent, CompletionError, CompletionModel, CompletionRequest,
            CompletionResponse, Usage,
        },
        streaming::StreamingCompletionResponse,
    };

    use super::*;
    use crate::llm::{
        ErrorKind, InputItem, InputSource, ModelOptions, Response, StreamEvent,
        protocol::openai_responses::{Reasoning, ReasoningEffort},
    };

    #[derive(Clone)]
    struct FakeModel {
        calls: Arc<AtomicUsize>,
    }

    impl CompletionModel for FakeModel {
        async fn completion(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, CompletionError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(CompletionResponse::new(
                vec![AssistantContent::text("ok")],
                Usage::default(),
                "fake",
            ))
        }

        async fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Result<StreamingCompletionResponse, CompletionError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(streaming(CompletionResponse::new(
                vec![AssistantContent::text("ok")],
                Usage::default(),
                "fake",
            )))
        }
    }

    /// Replay a response through a stream, the way Rig normalizes a provider
    /// stream into an aggregated response.
    fn streaming(response: CompletionResponse) -> StreamingCompletionResponse {
        use rig_core::streaming::{
            RawStreamingChoice, RawStreamingToolCall, StreamFinal, StreamPartId, WireId,
            normalize_stream,
        };
        let mut items = response
            .choice
            .into_iter()
            .map(|item| {
                Ok(match item {
                    AssistantContent::Text(text) => RawStreamingChoice::Message(text.text),
                    AssistantContent::ToolCall(call) => {
                        let provider = call.provider.as_ref();
                        let call_id = provider.map_or_else(
                            || call.id.as_str().to_owned(),
                            |provider| provider.call_id.clone(),
                        );
                        let item_id = provider.and_then(|provider| provider.item_id.clone());
                        let mut streamed = RawStreamingToolCall::new(
                            StreamPartId::wire(call_id.clone()),
                            call.function.name.clone(),
                            call.function.arguments.clone(),
                        )
                        .with_call_id(call_id);
                        streamed.tool_id = item_id.and_then(WireId::new);
                        RawStreamingChoice::ToolCall(streamed)
                    }
                    AssistantContent::Reasoning(reasoning) => RawStreamingChoice::Message(
                        reasoning
                            .content
                            .iter()
                            .filter_map(|part| match part {
                                rig_core::completion::message::ReasoningContent::Text {
                                    text,
                                    ..
                                } => Some(text.clone()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join(" "),
                    ),
                    AssistantContent::Image(_) => {
                        panic!("an image cannot be replayed through a stream fixture")
                    }
                })
            })
            .collect::<Vec<_>>();
        items.push(Ok(RawStreamingChoice::FinalResponse(StreamFinal::new(
            "fake",
            response.usage,
        ))));
        let stream = normalize_stream(Box::pin(stream::iter(items)), Ok);
        StreamingCompletionResponse::stream("fake", stream)
    }

    /// Drive a call to its single terminal response.
    async fn complete(model: &Model, request: Request) -> Result<Response, Error> {
        let mut stream = model.stream(request).await?;
        let mut terminal = None;
        while let Some(item) = stream.next().await {
            if let StreamEvent::Completed(response) = item? {
                terminal = Some(response);
            }
        }
        terminal.ok_or_else(Error::incomplete_stream)
    }

    fn model(calls: Arc<AtomicUsize>) -> Model {
        Model::new(
            Arc::from("fake-endpoint"),
            Protocol::OpenAiResponses,
            Arc::from("fake-model"),
            FakeModel { calls },
        )
    }

    #[tokio::test]
    async fn exposes_one_bone_completion_path() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model = model(Arc::clone(&calls));
        let response = complete(
            &model,
            Request::new([InputItem::external(InputSource::User, "hello")]),
        )
        .await
        .unwrap();

        assert_eq!(response.text().as_deref(), Some("ok"));
        assert_eq!(response.origin().endpoint_id(), "fake-endpoint");
        assert_eq!(response.origin().requested_model_id(), "fake-model");
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn validates_before_dispatch() {
        let calls = Arc::new(AtomicUsize::new(0));
        let error = complete(&model(Arc::clone(&calls)), Request::new([]))
            .await
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::InvalidRequest);
        assert_eq!(calls.load(Ordering::Relaxed), 0);

        // A reasoning effort is meaningful only on the Responses contract, and
        // an explicit request option must never be dropped silently.
        let anthropic = Model::new(
            Arc::from("fake-endpoint"),
            Protocol::AnthropicMessages,
            Arc::from("fake-model"),
            FakeModel {
                calls: Arc::clone(&calls),
            },
        );
        let error = complete(
            &anthropic,
            Request::new([InputItem::external(InputSource::User, "hello")]).options(
                ModelOptions::OpenAiResponses {
                    reasoning: Reasoning::new().effort(ReasoningEffort::High),
                },
            ),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::UnsupportedOption);
        assert_eq!(calls.load(Ordering::Relaxed), 0);

        // An option that carries no actual control is a malformed request, not
        // an endpoint limitation.
        let error = complete(
            &model(Arc::clone(&calls)),
            Request::new([InputItem::external(InputSource::User, "hello")]).options(
                ModelOptions::OpenAiResponses {
                    reasoning: Reasoning::new(),
                },
            ),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidRequest);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn incomplete_stream_is_an_explicit_terminal_error() {
        /// A provider stream that ends without a terminal record.
        #[derive(Clone)]
        struct EmptyStream;

        impl CompletionModel for EmptyStream {
            async fn completion(
                &self,
                _request: CompletionRequest,
            ) -> Result<CompletionResponse, CompletionError> {
                unreachable!("the streaming path is the only one")
            }

            async fn stream(
                &self,
                _request: CompletionRequest,
            ) -> Result<StreamingCompletionResponse, CompletionError> {
                let inner: rig_core::streaming::StreamingResult = Box::pin(stream::empty());
                Ok(StreamingCompletionResponse::stream("fake", inner))
            }
        }

        let model = Model::new(
            Arc::from("fake-endpoint"),
            Protocol::OpenAiResponses,
            Arc::from("fake-model"),
            EmptyStream,
        );
        let mut stream = model
            .stream(Request::new([InputItem::external(
                InputSource::User,
                "hello",
            )]))
            .await
            .unwrap();

        let error = stream.next().await.unwrap().unwrap_err();
        assert_eq!(error.kind(), ErrorKind::IncompleteStream);
        assert!(stream.next().await.is_none());
    }

    /// An image cannot survive BONE's response conversion, and the failure is
    /// explicit rather than a silently dropped item. This is a conversion
    /// boundary, so it is asserted at the conversion.
    #[tokio::test]
    async fn unsupported_image_output_is_an_explicit_protocol_error() {
        let origin = Arc::new(RequestOrigin {
            endpoint_id: Arc::from("fake-endpoint"),
            protocol: Protocol::OpenAiResponses,
            model_id: Arc::from("fake-model"),
        });
        let response = CompletionResponse::new(
            vec![AssistantContent::Image(Default::default())],
            Usage::default(),
            "fake",
        );

        let error = Response::from_rig(origin, response)
            .expect_err("image output must not disappear from a successful response");

        assert_eq!(error.kind(), ErrorKind::Protocol);
        assert!(error.to_string().contains("cannot represent"));
    }
}
