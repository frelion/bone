# Provider API

`bone-provider` is BONE's thin LLM protocol boundary. Rig owns normalized
messages, completion requests and responses, tools, provider-native state, and
streaming. BONE deliberately does not mirror those types.

The public model has three parts:

```text
Protocol                 Endpoint                         Model
wire contract      +     configured service       +      selected model
openai-responses         gateway-a                        model-x
openai-chat-completions  gateway-a                        model-y
anthropic-messages       anthropic-primary                claude-*
```

- [`Protocol`](../crates/bone-provider/src/protocol/mod.rs) identifies the wire
  contract. OpenAI Responses, OpenAI Chat Completions, and Anthropic Messages
  are distinct protocols.
- [`Endpoint`](../crates/bone-provider/src/endpoint.rs) is an application-named
  service instance with authentication, a base URL, and a model factory.
- [`Model`](../crates/bone-provider/src/model.rs) is a cloneable, type-erased
  Rig completion model carrying endpoint, protocol, and model identities.

Two gateways speaking OpenAI Responses are two endpoints backed by one
protocol implementation. They are not two Rust provider modules.

## OpenAI Responses

```rust,no_run
use bone_provider::{
    protocol::openai_responses::{
        self, Reasoning, ReasoningEffort, reasoning_params,
    },
    rig::message::Message,
};

# async fn example(api_key: String) -> Result<(), Box<dyn std::error::Error>> {
let endpoint = openai_responses::official("openai-primary", api_key)?;
let model = endpoint.model("your-responses-model")?;

assert_eq!(model.endpoint_id(), "openai-primary");
assert_eq!(model.protocol().as_str(), "openai-responses");

let mut stream = model
    .request(Message::user("Hello"))
    .additional_params(reasoning_params(
        Reasoning::new().with_effort(ReasoningEffort::High),
    ))
    .stream()
    .await?;

// `stream` is Rig's native `StreamingCompletionResponse`.
# use futures_util::StreamExt;
# while stream.next().await.is_some() {}
# Ok(())
# }
```

For an OpenAI Responses-compatible API root:

```rust,no_run
use bone_provider::protocol::openai_responses;

# fn example(api_key: String) -> Result<(), Box<dyn std::error::Error>> {
let endpoint = openai_responses::compatible(
    "gateway-a",
    api_key,
    "https://gateway.example/v1",
)?;
let model = endpoint.model("vendor-model")?;
# Ok(())
# }
```

The base URL is the prefix to which `/responses` is appended. An endpoint that
only implements `/chat/completions` does not implement this protocol and is not
silently treated as equivalent.

Compatible base URLs must be absolute HTTP(S) URLs without a query string or
embedded `user:password@host` credentials. A gateway that requires query-based
authentication or routing needs a custom `HttpClientExt` or Rig provider
extension that rewrites the final request URI; pass that configured client
through `openai_responses::from_client`. Merely putting a query string or
credential in Rig's base URL is unsupported and risks leaking secrets into URI
telemetry.

## OpenAI Chat Completions

Chat Completions is a separate wire contract, not a compatibility mode for
Responses:

```rust,no_run
use bone_provider::{
    protocol::openai_chat_completions,
    rig::message::Message,
};

# async fn example(api_key: String) -> Result<(), Box<dyn std::error::Error>> {
let endpoint = openai_chat_completions::official("openai-chat", api_key)?;
let model = endpoint.model("your-chat-completions-model")?;

assert_eq!(model.endpoint_id(), "openai-chat");
assert_eq!(model.protocol().as_str(), "openai-chat-completions");

let response = model
    .request(Message::user("Hello"))
    .max_tokens(256)
    .send()
    .await?;
# Ok(())
# }
```

For a compatible API root, use the Chat-specific constructor:

```rust,no_run
use bone_provider::protocol::openai_chat_completions;

# fn example(api_key: String) -> Result<(), Box<dyn std::error::Error>> {
let endpoint = openai_chat_completions::compatible(
    "chat-gateway",
    api_key,
    "https://gateway.example/v1",
)?;
let model = endpoint.model("vendor-chat-model")?;
# Ok(())
# }
```

This base URL is the prefix to which `/chat/completions` is appended. The same
absolute HTTP(S), no-query, no-embedded-credentials rule applies as for
Responses. Query-based routing requires a custom `HttpClientExt` or Rig
provider extension that constructs the final URI, injected through
`openai_chat_completions::from_client`.

An OpenAI-compatible service may implement `/responses`, `/chat/completions`,
or both. Supporting either path says nothing about support for the other. Pick
the constructor that matches the service's documented wire protocol; BONE
never probes one and silently falls back to the other.

## Anthropic Messages

```rust,no_run
use bone_provider::{
    protocol::anthropic_messages,
    rig::message::Message,
};

# async fn example(api_key: String) -> Result<(), Box<dyn std::error::Error>> {
let endpoint = anthropic_messages::official("anthropic-primary", api_key)?;
let model = endpoint.model("claude-model")?;

assert_eq!(model.endpoint_id(), "anthropic-primary");
assert_eq!(model.protocol().as_str(), "anthropic-messages");

let response = model
    .request(Message::user("Hello"))
    .max_tokens(256)
    .send()
    .await?;
# Ok(())
# }
```

`anthropic_messages::compatible` accepts a Messages-compatible base URL. Rig
normalizes a trailing `/v1`, `/messages`, or `/v1/messages` and sends the final
request to `/v1/messages` exactly once.

The same base-URL rule applies: it must be an absolute HTTP(S) URL without a
query string or embedded credentials. Query-based routing requires a custom
`HttpClientExt` or Rig provider extension that rewrites the final URI, injected
through `anthropic_messages::from_client`; a query-bearing ordinary base URL is
not sufficient.

## Custom clients and model options

Each protocol module exposes `from_client`. Use it for custom authentication
headers, other custom headers, a custom base URL, or a custom Rig
`HttpClientExt` implementation. Keep credentials in headers or transport
configuration, never in the URL. The transport's generic type is erased when
the endpoint is built.

Protocol-specific model options belong in that protocol module's
`from_model_factory` escape hatch. For example, an Anthropic caller can build
Rig models with prompt caching enabled without adding a generic JSON options
bag to BONE:

```rust,no_run
use bone_provider::{
    protocol::anthropic_messages,
    rig::{client::CompletionClient, providers::anthropic},
};

# fn example(api_key: String) -> Result<(), Box<dyn std::error::Error>> {
let client = anthropic::Client::new(api_key)?;
let endpoint = anthropic_messages::from_model_factory(
    "anthropic-cached",
    move |model_id| {
        client
            .completion_model(model_id)
            .with_automatic_caching()
    },
)?;
# Ok(())
# }
```

BONE does not read environment variables or persist credentials. Constructors
receive resolved values explicitly. Local construction failures use
`ConfigError`; network, protocol, and model failures remain Rig
`CompletionError` values.

## What belongs elsewhere

The future runtime/config layer owns configuration deserialization, secret
resolution, endpoint registries, routing, retries, fallback, rate limiting,
budgets, and pricing. None of those policies belong in `bone-provider`.

Adding a standard compatible service should normally require only new runtime
configuration and live certification. Add a new `Protocol` variant and module
only when the URL, headers, request/response JSON, or stream semantics form a
genuinely different wire contract.

## Inspect the OpenAI boundary

The example prints endpoint identity, protocol identity, the normalized Rig
request, stream events, and the final aggregated assistant choice:

```sh
export OPENAI_API_KEY='...'
export BONE_OPENAI_MODEL='...'
# Optional for a compatible endpoint:
export OPENAI_BASE_URL='https://gateway.example/v1'

cargo run -p bone-provider --example openai_responses_probe -- text
cargo run -p bone-provider --example openai_responses_probe -- tool
```

Tool mode defines a harmless fictional `inspect_path` function and displays
the model's call. It deliberately does not execute the tool or access the
filesystem.

Chat Completions has its own probe and model variable. It calls
`/chat/completions` and never falls back to Responses:

```sh
export OPENAI_API_KEY='...'
export BONE_OPENAI_CHAT_MODEL='...'
# Optional for a compatible endpoint:
export OPENAI_BASE_URL='https://gateway.example/v1'

cargo run -p bone-provider --example openai_chat_completions_probe -- text
cargo run -p bone-provider --example openai_chat_completions_probe -- tool
```

The two OpenAI probes reuse `OPENAI_API_KEY` and `OPENAI_BASE_URL` because one
run represents one configured OpenAI-compatible service. Their model variables
remain separate: `BONE_OPENAI_MODEL` selects a Responses-capable model, while
`BONE_OPENAI_CHAT_MODEL` selects a Chat Completions-capable model. Set only the
one the service supports. To certify services with different URLs or
credentials, run the probes separately with the corresponding environment.

The Anthropic probe exposes the same text/tool boundary for Messages:

```sh
export ANTHROPIC_API_KEY='...'
export BONE_ANTHROPIC_MODEL='...'
# Optional for a compatible endpoint:
export ANTHROPIC_BASE_URL='https://gateway.example'

cargo run -p bone-provider --example anthropic_messages_probe -- text
cargo run -p bone-provider --example anthropic_messages_probe -- tool
```
