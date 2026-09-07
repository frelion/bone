use bone_agent::{AgentModels, ConfiguredModel, ConfiguredModelError};
use bone_llm::protocol::openai_responses::{Reasoning, ReasoningEffort};
use bone_llm::{Model, ModelOptions, Protocol, testing};
use rig_core::{
    completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse},
    streaming::StreamingCompletionResponse,
};

#[derive(Clone)]
struct UnusedModel;

impl CompletionModel for UnusedModel {
    async fn completion(
        &self,
        _: CompletionRequest,
    ) -> Result<CompletionResponse, CompletionError> {
        panic!("fingerprint tests never dispatch a model request")
    }

    async fn stream(
        &self,
        _: CompletionRequest,
    ) -> Result<StreamingCompletionResponse, CompletionError> {
        panic!("fingerprint tests never dispatch a model request")
    }
}

fn model(endpoint: &str, protocol: Protocol, id: &str) -> Model {
    testing::model(endpoint, protocol, id, UnusedModel).unwrap()
}

#[test]
fn host_accepts_independently_constructed_role_models() {
    let models = AgentModels::new(
        ConfiguredModel::without_options(model(
            "review-provider",
            Protocol::OpenAiChatCompletions,
            "review-model",
        )),
        ConfiguredModel::without_options(model(
            "solver-provider",
            Protocol::AnthropicMessages,
            "solve-model",
        )),
    );

    assert_eq!(
        models.coordinator().model().endpoint_id(),
        "review-provider"
    );
    assert_eq!(
        models.coordinator().model().protocol(),
        Protocol::OpenAiChatCompletions
    );
    assert_eq!(models.solver().model().endpoint_id(), "solver-provider");
    assert_eq!(
        models.solver().model().protocol(),
        Protocol::AnthropicMessages
    );
}

#[test]
fn configured_model_rejects_options_for_another_protocol() {
    let options = ModelOptions::OpenAiResponses {
        reasoning: Reasoning::new().effort(ReasoningEffort::High),
    };
    let error = ConfiguredModel::new(
        model("anthropic", Protocol::AnthropicMessages, "claude-test"),
        Some(options),
    )
    .unwrap_err();

    assert_eq!(
        error,
        ConfiguredModelError::UnsupportedOptions {
            options_protocol: Protocol::OpenAiResponses,
            model_protocol: Protocol::AnthropicMessages,
        }
    );
}
