use std::{fmt, sync::Arc};

use rig_core::completion::CompletionModel;

use crate::{ConfigError, Model, Protocol, error::validate_endpoint_id};

type ModelFactory = dyn Fn(String) -> Model + Send + Sync;

/// A configured remote service that can select protocol-backed models.
///
/// An endpoint carries service identity separately from protocol identity. Two
/// gateways can therefore speak the same protocol without being represented
/// as two provider implementations.
#[derive(Clone)]
pub struct Endpoint {
    id: Arc<str>,
    protocol: Protocol,
    model_factory: Arc<ModelFactory>,
}

impl Endpoint {
    /// The application-defined identity of this configured service.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The wire protocol implemented by this endpoint.
    pub fn protocol(&self) -> Protocol {
        self.protocol
    }

    /// Select a model exposed by this endpoint.
    pub fn model(&self, model_id: impl Into<String>) -> Result<Model, ConfigError> {
        let model_id = model_id.into();
        crate::error::validate_model_id(&model_id)?;
        Ok((self.model_factory)(model_id))
    }

    /// Build an endpoint from a concrete Rig model factory, erasing both the
    /// factory and model types at this boundary.
    ///
    /// This stays crate-private so protocol modules remain responsible for
    /// assigning the correct protocol identity.
    pub(crate) fn from_model_factory<F, M>(
        endpoint_id: impl Into<String>,
        protocol: Protocol,
        factory: F,
    ) -> Result<Self, ConfigError>
    where
        F: Fn(String) -> M + Send + Sync + 'static,
        M: CompletionModel + Send + Sync + 'static,
    {
        let endpoint_id = endpoint_id.into();
        validate_endpoint_id(&endpoint_id)?;

        let id: Arc<str> = Arc::from(endpoint_id);
        let model_endpoint_id = Arc::clone(&id);
        let model_factory = Arc::new(move |model_id: String| {
            let inner = factory(model_id.clone());
            Model::new(
                Arc::clone(&model_endpoint_id),
                protocol,
                Arc::from(model_id),
                inner,
            )
        });

        Ok(Self {
            id,
            protocol,
            model_factory,
        })
    }
}

impl fmt::Debug for Endpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Endpoint")
            .field("id", &self.id)
            .field("protocol", &self.protocol)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use rig_core::{
        completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse},
        streaming::StreamingCompletionResponse,
    };

    use super::*;

    #[derive(Clone)]
    struct FakeModel;

    impl CompletionModel for FakeModel {
        async fn completion(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, CompletionError> {
            unreachable!("construction test does not dispatch requests")
        }

        async fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Result<StreamingCompletionResponse, CompletionError> {
            unreachable!("construction test does not dispatch requests")
        }
    }

    #[test]
    fn keeps_endpoint_protocol_and_model_identities_separate() {
        let endpoint =
            Endpoint::from_model_factory("gateway-a", Protocol::OpenAiResponses, |_model_id| {
                FakeModel
            })
            .unwrap();
        let model = endpoint.model("model-x").unwrap();

        assert_eq!(endpoint.id(), "gateway-a");
        assert_eq!(endpoint.protocol(), Protocol::OpenAiResponses);
        assert_eq!(model.endpoint_id(), "gateway-a");
        assert_eq!(model.protocol(), Protocol::OpenAiResponses);
        assert_eq!(model.id(), "model-x");
    }

    #[test]
    fn rejects_empty_endpoint_and_model_identities() {
        let error =
            Endpoint::from_model_factory("  ", Protocol::OpenAiResponses, |_model_id| FakeModel)
                .unwrap_err();
        assert_eq!(error, ConfigError::EmptyEndpointId);

        let endpoint =
            Endpoint::from_model_factory("gateway-a", Protocol::OpenAiResponses, |_model_id| {
                FakeModel
            })
            .unwrap();
        assert_eq!(endpoint.model("  ").unwrap_err(), ConfigError::EmptyModelId);
    }
}
