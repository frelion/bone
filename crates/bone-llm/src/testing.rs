//! Explicit test-only construction seams for BONE's own contract tests.

use std::fmt::Debug;

use rig_core::{
    completion::CompletionModel,
    http_client::HttpClientExt,
    providers::{anthropic as rig_anthropic, chatgpt as rig_chatgpt, openai as rig_openai},
    wasm_compat::{WasmCompatSend, WasmCompatSync},
};

use crate::{ConfigError, Endpoint, Error, ErrorKind, Model, Protocol, model::RequestSupport};

pub fn error(kind: ErrorKind, message: impl Into<String>) -> Error {
    Error::new(kind, message)
}

pub fn model<M>(
    endpoint_id: impl Into<String>,
    protocol: Protocol,
    model_id: impl Into<String>,
    inner: M,
) -> Result<Model, ConfigError>
where
    M: CompletionModel + Clone + Send + Sync + 'static,
{
    let endpoint = Endpoint::from_model_factory(endpoint_id, protocol, move |_| inner.clone())?;
    endpoint.model(model_id)
}

pub fn openai_responses_endpoint<H>(
    endpoint_id: impl Into<String>,
    client: rig_openai::Client<H>,
) -> Result<Endpoint, ConfigError>
where
    H: HttpClientExt + Clone + Default + Debug + Send + Sync + 'static,
{
    crate::protocol::openai_responses::from_client(endpoint_id, client)
}

pub fn openai_chat_completions_endpoint<H>(
    endpoint_id: impl Into<String>,
    client: rig_openai::CompletionsClient<H>,
) -> Result<Endpoint, ConfigError>
where
    H: HttpClientExt + Clone + Default + Debug + Send + Sync + 'static,
{
    crate::protocol::openai_chat_completions::from_client(endpoint_id, client)
}

pub fn anthropic_messages_endpoint<H>(
    endpoint_id: impl Into<String>,
    client: rig_anthropic::Client<H>,
) -> Result<Endpoint, ConfigError>
where
    H: HttpClientExt + Clone + Default + Send + Sync + 'static,
{
    crate::protocol::anthropic_messages::from_client(endpoint_id, client)
}

pub fn chatgpt_subscription_endpoint<H>(
    endpoint_id: impl Into<String>,
    client: rig_chatgpt::Client<H>,
) -> Result<Endpoint, ConfigError>
where
    H: HttpClientExt
        + Clone
        + Default
        + Debug
        + WasmCompatSend
        + WasmCompatSync
        + Send
        + Sync
        + 'static,
{
    crate::service::chatgpt_subscription::from_unmanaged_client(endpoint_id, client)
}

pub fn chatgpt_model<M>(
    endpoint_id: impl Into<String>,
    protocol: Protocol,
    model_id: impl Into<String>,
    inner: M,
) -> Result<Model, ConfigError>
where
    M: CompletionModel + Clone + Send + Sync + 'static,
{
    let endpoint = Endpoint::from_model_factory_with_support(
        endpoint_id,
        protocol,
        RequestSupport::CHATGPT_SUBSCRIPTION,
        move |_| inner.clone(),
    )?;
    endpoint.model(model_id)
}
