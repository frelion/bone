use bone_core::{
    CallContext, CallError, CallErrorKind, CheckpointDraft, CompactInput, ConversationInput,
    ConversationStep, ModelPort, PortFuture, WorkInput, WorkProposal, model_contract,
};

use crate::llm::{
    InputItem, InputSource, Model, ModelOptions, ModelOptionsError, Request, Response, ToolChoice,
    ToolDefinition,
};

/// A connected model with protocol-specific request defaults already checked.
#[derive(Clone, Debug)]
pub struct ConfiguredModel {
    model: Model,
    options: Option<ModelOptions>,
}

impl ConfiguredModel {
    pub fn new(model: Model, options: Option<ModelOptions>) -> Result<Self, ModelOptionsError> {
        if let Some(options) = options.as_ref() {
            options.validate_for_protocol(model.protocol())?;
        }
        Ok(Self { model, options })
    }

    pub fn without_options(model: Model) -> Self {
        Self {
            model,
            options: None,
        }
    }

    fn model(&self) -> &Model {
        &self.model
    }

    fn apply_to(&self, request: Request) -> Request {
        match &self.options {
            Some(options) => request.options(options.clone()),
            None => request,
        }
    }
}

impl From<Model> for ConfiguredModel {
    fn from(model: Model) -> Self {
        Self::without_options(model)
    }
}

/// Adapts configured provider models to the core's typed model port.
#[derive(Clone)]
pub struct ModelAdapter {
    coordinator: ConfiguredModel,
    worker: ConfiguredModel,
}

impl ModelAdapter {
    pub fn new(
        coordinator: impl Into<ConfiguredModel>,
        worker: impl Into<ConfiguredModel>,
    ) -> Self {
        Self {
            coordinator: coordinator.into(),
            worker: worker.into(),
        }
    }
}

impl ModelPort for ModelAdapter {
    fn converse(
        &self,
        input: ConversationInput,
        context: CallContext,
    ) -> PortFuture<Result<ConversationStep, CallError>> {
        invoke(self.coordinator.clone(), context, move || {
            model_contract::converse(input)
        })
    }

    fn work(
        &self,
        input: WorkInput,
        context: CallContext,
    ) -> PortFuture<Result<WorkProposal, CallError>> {
        invoke(self.worker.clone(), context, move || {
            model_contract::work(input)
        })
    }

    fn compact(
        &self,
        input: CompactInput,
        context: CallContext,
    ) -> PortFuture<Result<CheckpointDraft, CallError>> {
        invoke(self.worker.clone(), context, move || {
            model_contract::compact(input)
        })
    }
}

fn invoke<O, F>(
    model: ConfiguredModel,
    context: CallContext,
    contract: F,
) -> PortFuture<Result<O, CallError>>
where
    O: Send + 'static,
    F: FnOnce() -> Result<model_contract::ModelCall<O>, CallError> + Send + 'static,
{
    Box::pin(async move { execute(model, context.cancellation_requested(), contract).await })
}

async fn execute<O, F>(
    model: ConfiguredModel,
    cancellation_requested: bool,
    contract: F,
) -> Result<O, CallError>
where
    O: Send + 'static,
    F: FnOnce() -> Result<model_contract::ModelCall<O>, CallError> + Send + 'static,
{
    if cancellation_requested {
        return Err(cancelled());
    }

    let contract = contract()?;
    let (context_name, context_json) = contract.context();
    let (submission_name, submission_description, submission_schema) = contract.submission();
    let submission_name = submission_name.to_owned();

    let request = Request::new([InputItem::external(
        InputSource::Named(context_name.into()),
        context_json,
    )])
    .instructions(contract.instructions())
    .tools([ToolDefinition::new(
        submission_name.clone(),
        submission_description,
        submission_schema.clone(),
    )])
    .tool_choice(ToolChoice::Specific(vec![submission_name.clone()]));
    let request = model.apply_to(request);
    let response = model.model().complete(request).await.map_err(|error| {
        CallError::failed(format!(
            "model request failed ({:?}): {error}",
            error.kind()
        ))
    })?;

    decode_response(contract, response)
}

fn decode_response<O>(
    contract: model_contract::ModelCall<O>,
    response: Response,
) -> Result<O, CallError> {
    let submission_name = contract.submission().0.to_owned();
    if response
        .finish_reason()
        .is_some_and(|reason| reason.truncated_output())
    {
        return Err(CallError::failed("model output was truncated or filtered"));
    }

    let calls = response.tool_calls().collect::<Vec<_>>();
    if calls.len() != 1 || calls[0].name() != submission_name {
        return Err(CallError::failed(format!(
            "model must return exactly one {submission_name} call"
        )));
    }

    contract.decode(calls[0].arguments())
}

fn cancelled() -> CallError {
    CallError {
        kind: CallErrorKind::Cancelled,
        message: "call cancelled".into(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use bone_core::{
        ConversationInput, ConversationStep, Input, InputId, SessionContext, model_contract,
    };
    use rig_core::{
        completion::{
            AssistantContent, CompletionError, CompletionModel, CompletionRequest,
            CompletionResponse, FinishReason, Usage,
        },
        message::{
            Message, ToolCall as RigToolCall, ToolChoice as RigToolChoice, ToolFunction,
            UserContent,
        },
        streaming::StreamingCompletionResponse,
    };
    use serde_json::{Value, json};

    use super::*;
    use crate::llm::{
        ModelOptions, Protocol, RequestOrigin, RequestSupport, ToolCallIdentities,
        protocol::openai_responses::{Reasoning, ReasoningEffort},
    };

    #[derive(Clone)]
    struct UnusedModel;

    impl CompletionModel for UnusedModel {
        async fn completion(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, CompletionError> {
            unreachable!("configuration tests do not dispatch")
        }

        async fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Result<StreamingCompletionResponse, CompletionError> {
            unreachable!("configuration tests do not dispatch")
        }
    }

    #[derive(Clone)]
    struct RecordingModel {
        requests: Arc<Mutex<Vec<CompletionRequest>>>,
        response: CompletionResponse,
    }

    impl CompletionModel for RecordingModel {
        async fn completion(
            &self,
            request: CompletionRequest,
        ) -> Result<CompletionResponse, CompletionError> {
            self.requests.lock().unwrap().push(request);
            Ok(self.response.clone())
        }

        async fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Result<StreamingCompletionResponse, CompletionError> {
            unreachable!("the agent adapter uses unary completion")
        }
    }

    #[derive(Clone)]
    struct FailingModel;

    impl CompletionModel for FailingModel {
        async fn completion(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, CompletionError> {
            Err(CompletionError::ProviderError("request rejected".into()))
        }

        async fn stream(
            &self,
            _request: CompletionRequest,
        ) -> Result<StreamingCompletionResponse, CompletionError> {
            unreachable!("the agent adapter uses unary completion")
        }
    }

    fn model(protocol: Protocol) -> Model {
        Model::new(
            Arc::from("test-endpoint"),
            protocol,
            Arc::from("test-model"),
            RequestSupport::FULL,
            UnusedModel,
        )
    }

    fn recording_model(
        requests: Arc<Mutex<Vec<CompletionRequest>>>,
        response: CompletionResponse,
    ) -> ConfiguredModel {
        Model::new(
            Arc::from("test-endpoint"),
            Protocol::OpenAiResponses,
            Arc::from("test-model"),
            RequestSupport::FULL,
            RecordingModel { requests, response },
        )
        .into()
    }

    fn submission(name: &str, arguments: Value) -> CompletionResponse {
        CompletionResponse::new(
            vec![AssistantContent::ToolCall(RigToolCall::from_dual_wire(
                "item-0",
                "call-0",
                ToolFunction::new(name.to_owned(), arguments),
            ))],
            Usage::default(),
            "test",
        )
        .with_finish_reason(FinishReason::ToolCalls)
    }

    fn conversation_input() -> ConversationInput {
        ConversationInput {
            tools: Vec::new(),
            constraints_revision: 0,
            inputs: vec![Input::new(InputId(2), "inspect the parser")],
            constraints: "read only".into(),
            background: Arc::new(SessionContext::default()),
            jobs: Vec::new(),
            next_job: None,
            records: Vec::new(),
        }
    }

    fn response(calls: impl IntoIterator<Item = (&'static str, Value)>) -> Response {
        response_with_finish(calls, None)
    }

    fn response_with_finish(
        calls: impl IntoIterator<Item = (&'static str, Value)>,
        finish: Option<FinishReason>,
    ) -> Response {
        let choices = calls
            .into_iter()
            .enumerate()
            .map(|(index, (name, arguments))| {
                AssistantContent::ToolCall(RigToolCall::from_dual_wire(
                    format!("item-{index}"),
                    format!("call-{index}"),
                    ToolFunction::new(name.to_owned(), arguments),
                ))
            })
            .collect();
        let response = CompletionResponse::new(choices, Usage::default(), "test")
            .with_optional_finish_reason(finish);
        Response::from_rig(
            Arc::new(RequestOrigin {
                endpoint_id: Arc::from("test-endpoint"),
                protocol: Protocol::OpenAiResponses,
                model_id: Arc::from("test-model"),
            }),
            response,
            ToolCallIdentities::default(),
        )
        .unwrap()
    }

    #[test]
    fn configured_model_rejects_options_for_another_protocol() {
        let options = ModelOptions::OpenAiResponses {
            reasoning: Reasoning::new().effort(ReasoningEffort::High),
        };

        let error = ConfiguredModel::new(model(Protocol::AnthropicMessages), Some(options))
            .expect_err("OpenAI options must not reach an Anthropic model");
        assert_eq!(
            error,
            ModelOptionsError::UnsupportedProtocol {
                options_protocol: Protocol::OpenAiResponses,
                endpoint_protocol: Protocol::AnthropicMessages,
            }
        );
    }

    #[tokio::test]
    async fn pre_cancelled_call_does_not_reach_the_provider() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let model = recording_model(
            Arc::clone(&requests),
            submission(
                "submit_conversation",
                json!(ConversationStep::Ask {
                    inputs: vec![InputId(2)],
                    question: "unused".into()
                }),
            ),
        );

        let error = execute::<ConversationStep, _>(model, true, || {
            panic!("a pre-cancelled call must not construct its contract")
        })
        .await
        .expect_err("a pre-cancelled call must stop locally");

        assert_eq!(error.kind, CallErrorKind::Cancelled);
        assert!(requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn provider_failures_become_core_call_failures() {
        let model = ConfiguredModel::from(Model::new(
            Arc::from("test-endpoint"),
            Protocol::OpenAiResponses,
            Arc::from("test-model"),
            RequestSupport::FULL,
            FailingModel,
        ));

        let error = execute(model, false, || {
            model_contract::converse(conversation_input())
        })
        .await
        .expect_err("provider failure must cross the port as a CallError");

        assert_eq!(error.kind, CallErrorKind::Failed);
        assert_eq!(
            error.message,
            "model request failed (Provider): request rejected"
        );
    }

    #[tokio::test]
    async fn request_carries_the_bound_contract_specific_choice_and_configured_options() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let decision = ConversationStep::Ask {
            inputs: vec![InputId(2)],
            question: "which parser?".into(),
        };
        let input = conversation_input();
        let expected_contract = model_contract::converse(input.clone()).unwrap();
        let expected_instructions = expected_contract.instructions().to_owned();
        let (expected_context_label, expected_context) = expected_contract.context();
        let expected_context_label = expected_context_label.to_owned();
        let expected_context = expected_context.to_owned();
        let (expected_name, expected_description, expected_schema) = expected_contract.submission();
        let expected_name = expected_name.to_owned();
        let expected_description = expected_description.to_owned();
        let expected_schema = expected_schema.clone();
        let model = recording_model(
            Arc::clone(&requests),
            submission("submit_conversation", json!({"step": decision})),
        );
        let model = ConfiguredModel::new(
            model.model().clone(),
            Some(ModelOptions::OpenAiResponses {
                reasoning: Reasoning::new().effort(ReasoningEffort::High),
            }),
        )
        .unwrap();

        let result = execute(model, false, move || model_contract::converse(input))
            .await
            .unwrap();
        assert_eq!(result, decision);

        let mut requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let request = requests.pop().unwrap();
        assert_eq!(
            request.additional_params,
            Some(json!({"reasoning": {"effort": "high"}}))
        );
        assert_eq!(request.tools.len(), 1);
        assert_eq!(request.tools[0].name, expected_name);
        assert_eq!(request.tools[0].description, expected_description);
        assert_eq!(
            request.tool_choice,
            Some(RigToolChoice::Specific {
                function_names: vec![request.tools[0].name.clone()]
            })
        );
        assert_eq!(request.tools[0].parameters, expected_schema);
        assert_eq!(request.chat_history.len(), 2);
        match &request.chat_history[0] {
            Message::System { content } => assert_eq!(content, &expected_instructions),
            other => panic!("expected system instructions, got {other:?}"),
        }
        match &request.chat_history[1] {
            Message::User { content } => match content.as_slice() {
                [UserContent::Text(text)] => assert_eq!(
                    text.text,
                    format!(
                        "<bone_external source=\"{expected_context_label}\">\n{expected_context}\n</bone_external>"
                    )
                ),
                other => panic!("expected one text context block, got {other:?}"),
            },
            other => panic!("expected serialized agent context, got {other:?}"),
        }
    }

    #[test]
    fn response_requires_exactly_one_expected_submission() {
        let responses = [
            response(std::iter::empty()),
            response([("wrong_submission", json!({"Clarify": "question"}))]),
            response([
                (
                    "submit_conversation",
                    json!({"decision": {"Clarify": "question"}}),
                ),
                (
                    "submit_conversation",
                    json!({"decision": {"Clarify": "question"}}),
                ),
            ]),
        ];

        for response in responses {
            let error = decode_response(
                model_contract::converse(conversation_input()).unwrap(),
                response,
            )
            .expect_err("invalid submission cardinality or name must fail");
            assert!(error.message.contains("exactly one submit_conversation"));
        }
    }

    #[test]
    fn response_rejects_truncation_and_filtering_before_decoding() {
        for finish_reason in [FinishReason::Length, FinishReason::ContentFilter] {
            let response = response_with_finish(
                [(
                    "submit_conversation",
                    json!({"decision": {"Clarify": "question"}}),
                )],
                Some(finish_reason),
            );

            let error = decode_response(
                model_contract::converse(conversation_input()).unwrap(),
                response,
            )
            .expect_err("truncated or filtered output must fail");
            assert_eq!(error.message, "model output was truncated or filtered");
        }
    }
}
