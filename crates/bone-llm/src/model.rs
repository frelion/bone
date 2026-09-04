use std::{fmt, future::Future, pin::Pin, sync::Arc};

use rig_core::{
    completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse},
    streaming::StreamingCompletionResponse,
};

use crate::{Error, Protocol, Request, Response, ResponseStream};

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
type CompletionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<CompletionResponse, CompletionError>> + Send + 'a>>;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
type CompletionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<CompletionResponse, CompletionError>> + 'a>>;

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
type StreamFuture<'a> =
    Pin<Box<dyn Future<Output = Result<StreamingCompletionResponse, CompletionError>> + Send + 'a>>;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
type StreamFuture<'a> =
    Pin<Box<dyn Future<Output = Result<StreamingCompletionResponse, CompletionError>> + 'a>>;

trait ErasedModel: Send + Sync {
    fn complete(&self, request: CompletionRequest) -> CompletionFuture<'_>;
    fn stream(&self, request: CompletionRequest) -> StreamFuture<'_>;
}

impl<M> ErasedModel for M
where
    M: CompletionModel + Send + Sync + 'static,
{
    fn complete(&self, request: CompletionRequest) -> CompletionFuture<'_> {
        Box::pin(CompletionModel::completion(self, request))
    }

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

impl RequestOrigin {
    pub(crate) fn ensure_same(&self, other: &Self) -> Result<(), Error> {
        if self == other {
            Ok(())
        } else {
            Err(Error::invalid(
                "assistant and tool state can only be replayed to the model that produced it",
            ))
        }
    }
}

/// Endpoint-specific request facts that prevent accepted options from being
/// silently discarded. Kept private rather than exposed as a capability
/// registry.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RequestSupport {
    pub(crate) max_output_tokens: bool,
    pub(crate) structured_output: bool,
}

impl RequestSupport {
    pub(crate) const FULL: Self = Self {
        max_output_tokens: true,
        structured_output: true,
    };

    pub(crate) const CHATGPT_SUBSCRIPTION: Self = Self {
        max_output_tokens: false,
        structured_output: false,
    };
}

/// A selected model behind one configured endpoint.
#[derive(Clone)]
pub struct Model {
    origin: Arc<RequestOrigin>,
    support: RequestSupport,
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
        support: RequestSupport,
        inner: impl CompletionModel + Send + Sync + 'static,
    ) -> Self {
        Self {
            origin: Arc::new(RequestOrigin {
                endpoint_id,
                protocol,
                model_id: id,
            }),
            support,
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

    /// Execute one complete request.
    pub async fn complete(&self, request: Request) -> Result<Response, Error> {
        let (request, previous_tool_calls) = request.into_rig(&self.origin, self.support)?;
        let response = self
            .inner
            .complete(request)
            .await
            .map_err(Error::from_rig)?;
        Response::from_rig(Arc::clone(&self.origin), response, previous_tool_calls)
    }

    /// Open one streaming request.
    pub async fn stream(&self, request: Request) -> Result<ResponseStream, Error> {
        let (request, previous_tool_calls) = request.into_rig(&self.origin, self.support)?;
        let stream = self.inner.stream(request).await.map_err(Error::from_rig)?;
        Ok(ResponseStream::new(
            stream,
            Arc::clone(&self.origin),
            previous_tool_calls,
        ))
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
        message::{ToolCall as RigToolCall, ToolFunction},
        streaming::{StreamingCompletionResponse, StreamingResult},
    };

    use super::*;
    use crate::{ErrorKind, InputItem, InputSource};

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
            let inner: StreamingResult = Box::pin(stream::empty());
            Ok(StreamingCompletionResponse::stream("fake", inner))
        }
    }

    fn model(calls: Arc<AtomicUsize>) -> Model {
        Model::new(
            Arc::from("fake-endpoint"),
            Protocol::OpenAiResponses,
            Arc::from("fake-model"),
            RequestSupport::FULL,
            FakeModel { calls },
        )
    }

    #[tokio::test]
    async fn exposes_one_bone_completion_path() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model = model(Arc::clone(&calls));
        let response = model
            .complete(Request::new([InputItem::external(
                InputSource::User,
                "hello",
            )]))
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
        let error = model(Arc::clone(&calls))
            .complete(Request::new([]))
            .await
            .unwrap_err();

        assert_eq!(error.kind(), ErrorKind::InvalidRequest);
        assert_eq!(calls.load(Ordering::Relaxed), 0);

        let error = model(Arc::clone(&calls))
            .complete(
                Request::new([InputItem::external(InputSource::User, "hello")])
                    .options(crate::protocol::openai_responses::Options::new()),
            )
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidRequest);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn incomplete_stream_is_an_explicit_terminal_error() {
        let mut stream = model(Arc::new(AtomicUsize::new(0)))
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

    #[derive(Clone)]
    struct ReusedProviderItemModel {
        calls: Arc<AtomicUsize>,
    }

    impl CompletionModel for ReusedProviderItemModel {
        async fn completion(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, CompletionError> {
            let turn = self.calls.fetch_add(1, Ordering::Relaxed);
            let call_id = if turn == 0 {
                "call-first"
            } else {
                "call-second"
            };
            let call = RigToolCall::from_dual_wire(
                "reused-provider-item",
                call_id,
                ToolFunction::new("inspect".to_owned(), serde_json::json!({})),
            );
            Ok(CompletionResponse::new(
                vec![AssistantContent::ToolCall(call)],
                Usage::default(),
                "fake",
            ))
        }

        async fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Result<StreamingCompletionResponse, CompletionError> {
            unreachable!("identity test uses unary completion")
        }
    }

    #[tokio::test]
    async fn rejects_provider_tool_identity_reuse_across_turns() {
        let model = Model::new(
            Arc::from("fake-endpoint"),
            Protocol::OpenAiResponses,
            Arc::from("fake-model"),
            RequestSupport::FULL,
            ReusedProviderItemModel {
                calls: Arc::new(AtomicUsize::new(0)),
            },
        );
        let user = InputItem::external(InputSource::User, "inspect");
        let first = model
            .complete(Request::new([user.clone()]))
            .await
            .expect("first tool identity is unique");
        let replay = first.into_item().expect("tool call is replayable");

        let error = model
            .complete(Request::new([
                user,
                replay,
                InputItem::external(InputSource::User, "inspect again"),
            ]))
            .await
            .expect_err("a provider item id cannot be reused");

        assert_eq!(error.kind(), ErrorKind::Protocol);
        assert!(error.to_string().contains("provider tool item identifier"));
    }

    #[derive(Clone)]
    struct ImageModel;

    impl CompletionModel for ImageModel {
        async fn completion(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, CompletionError> {
            Ok(CompletionResponse::new(
                vec![AssistantContent::Image(Default::default())],
                Usage::default(),
                "fake",
            ))
        }

        async fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Result<StreamingCompletionResponse, CompletionError> {
            unreachable!("image boundary test uses unary completion")
        }
    }

    #[tokio::test]
    async fn unsupported_image_output_is_an_explicit_protocol_error() {
        let model = Model::new(
            Arc::from("fake-endpoint"),
            Protocol::OpenAiResponses,
            Arc::from("fake-model"),
            RequestSupport::FULL,
            ImageModel,
        );

        let error = model
            .complete(Request::new([InputItem::external(
                InputSource::User,
                "make an image",
            )]))
            .await
            .expect_err("image output must not disappear from a successful response");

        assert_eq!(error.kind(), ErrorKind::Protocol);
        assert!(error.to_string().contains("cannot represent"));
    }
}
