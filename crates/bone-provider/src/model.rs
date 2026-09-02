use std::{fmt, future::Future, pin::Pin, sync::Arc};

use rig_core::{
    completion::{
        CompletionError, CompletionModel, CompletionRequest, CompletionRequestBuilder,
        CompletionResponse, ProviderCapabilities,
    },
    message::Message,
    streaming::StreamingCompletionResponse,
};

use crate::Protocol;

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
    fn capabilities(&self) -> ProviderCapabilities;
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

    fn capabilities(&self) -> ProviderCapabilities {
        CompletionModel::capabilities(self)
    }
}

/// A selected Rig model with its concrete provider type erased.
///
/// Rig's [`CompletionModel`] uses return-position `impl Future`, so the trait
/// cannot itself be used as `dyn CompletionModel`. This handle keeps that
/// concrete model type out of runtime code while preserving Rig's native
/// request and response types.
#[derive(Clone)]
pub struct Model {
    endpoint_id: Arc<str>,
    protocol: Protocol,
    id: Arc<str>,
    inner: Arc<dyn ErasedModel>,
}

impl fmt::Debug for Model {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Model")
            .field("endpoint_id", &self.endpoint_id)
            .field("protocol", &self.protocol)
            .field("id", &self.id)
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
            endpoint_id,
            protocol,
            id,
            inner: Arc::new(inner),
        }
    }

    /// The application-defined identity of the endpoint that selected this model.
    pub fn endpoint_id(&self) -> &str {
        &self.endpoint_id
    }

    /// The wire protocol used by this model.
    pub fn protocol(&self) -> Protocol {
        self.protocol
    }

    /// The configured model identifier.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Start a native Rig request builder for this model.
    pub fn request(&self, prompt: impl Into<Message>) -> CompletionRequestBuilder<Self> {
        CompletionRequestBuilder::new(self.clone(), prompt)
    }

    /// Execute one non-streaming Rig request.
    pub async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, CompletionError> {
        request.validate_message_content()?;
        self.inner.complete(request).await
    }

    /// Execute one streaming Rig request.
    pub async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse, CompletionError> {
        request.validate_message_content()?;
        self.inner.stream(request).await
    }

    /// Provider behavior relevant while preparing requests.
    pub fn capabilities(&self) -> ProviderCapabilities {
        self.inner.capabilities()
    }
}

impl CompletionModel for Model {
    async fn completion(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, CompletionError> {
        self.complete(request).await
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse, CompletionError> {
        self.stream(request).await
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.capabilities()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use futures_util::stream;
    use rig_core::{
        completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse},
        message::Message,
        streaming::{StreamingCompletionResponse, StreamingResult},
    };

    use super::Model;
    use crate::Protocol;

    #[derive(Clone)]
    struct FakeModel {
        calls: Arc<AtomicUsize>,
    }

    impl CompletionModel for FakeModel {
        async fn completion(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, CompletionError> {
            Err(CompletionError::ResponseError(
                "non-streaming path is not scripted".to_owned(),
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

    #[tokio::test]
    async fn erases_a_rig_model_without_changing_its_stream() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model = Model::new(
            Arc::from("fake-endpoint"),
            Protocol::OpenAiResponses,
            Arc::from("fake-model"),
            FakeModel {
                calls: Arc::clone(&calls),
            },
        );

        let stream = model
            .request(Message::user("hello"))
            .stream()
            .await
            .unwrap();

        assert_eq!(model.endpoint_id(), "fake-endpoint");
        assert_eq!(model.protocol(), Protocol::OpenAiResponses);
        assert_eq!(model.id(), "fake-model");
        assert_eq!(stream.provider(), "fake");
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn direct_calls_validate_rig_requests_before_dispatch() {
        let calls = Arc::new(AtomicUsize::new(0));
        let model = Model::new(
            Arc::from("fake-endpoint"),
            Protocol::OpenAiResponses,
            Arc::from("fake-model"),
            FakeModel {
                calls: Arc::clone(&calls),
            },
        );
        let request = empty_request();

        let error = match model.stream(request).await {
            Ok(_) => panic!("empty request unexpectedly reached the model"),
            Err(error) => error,
        };

        assert!(matches!(error, CompletionError::RequestError(_)));
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    fn empty_request() -> CompletionRequest {
        CompletionRequest {
            model: None,
            preamble: None,
            chat_history: Vec::new(),
            documents: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            tool_choice: None,
            additional_params: None,
            output_schema: None,
            record_telemetry_content: false,
        }
    }
}
