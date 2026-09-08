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

    assert_eq!(models.kernel().model().endpoint_id(), "review-provider");
    assert_eq!(
        models.kernel().model().protocol(),
        Protocol::OpenAiChatCompletions
    );
    assert_eq!(models.worker().model().endpoint_id(), "solver-provider");
    assert_eq!(
        models.worker().model().protocol(),
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

#[test]
fn scheduling_defaults_are_bounded_independently() {
    let config = bone_agent::KernelConfig::default();
    assert_eq!(config.background_concurrency, 2);
    assert_eq!(config.input_capacity, 32);
    assert_eq!(config.tool_concurrency, 8);
    assert!(!config.kernel_timeout.is_zero());
    assert!(!config.work_timeout.is_zero());
}

#[test]
fn zero_limits_and_duplicate_tool_registrations_are_rejected() {
    use bone_agent::{Kernel, KernelConfig, ToolEffect, ToolSpec};
    use std::time::Duration;
    for config in [
        KernelConfig {
            kernel_timeout: Duration::ZERO,
            ..Default::default()
        },
        KernelConfig {
            work_timeout: Duration::ZERO,
            ..Default::default()
        },
        KernelConfig {
            background_concurrency: 0,
            ..Default::default()
        },
        KernelConfig {
            input_capacity: 0,
            ..Default::default()
        },
        KernelConfig {
            tool_concurrency: 0,
            ..Default::default()
        },
    ] {
        assert!(Kernel::new(config, vec![]).is_err());
    }
    let spec = ToolSpec {
        name: "duplicate".into(),
        description: "tool".into(),
        parameters: serde_json::json!({"type":"object"}),
        effect: ToolEffect::ReadOnly,
    };
    assert!(Kernel::new(KernelConfig::default(), vec![spec.clone(), spec]).is_err());
}
