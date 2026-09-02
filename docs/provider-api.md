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

## Experimental ChatGPT subscription service

The experimental `chatgpt_subscription` service adapter provides in-process
access to the Codex Responses backend using a ChatGPT subscription:

```rust,no_run
use bone_provider::{
    rig::message::Message,
    service::chatgpt_subscription,
};

# fn show_device_login(_url: String, _code: String) {}
# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let endpoint = chatgpt_subscription::connect("chatgpt-subscription", |prompt| {
    // Render only in the active connection UI; do not log or persist the code.
    show_device_login(prompt.verification_uri, prompt.user_code);
}).await?;
let model = endpoint.model("a-model-available-to-your-subscription")?;
let response = model.request(Message::user("Hello")).send().await?;
# Ok(())
# }
```

This is a service adapter, not a fourth protocol. Rig sends native Responses
requests and SSE events, so the endpoint and selected model identify as
`Protocol::OpenAiResponses`; the normalized response provider is `chatgpt`.
The service-specific URL, subscription headers, OAuth lifecycle, forced SSE,
and request restrictions stay inside Rig's ChatGPT provider.

The adapter does not run a local proxy and does not invoke the Codex agent.
The explicit `connect` call reports a device-login URL and code through the
provided callback. The library never prints the code itself. Once the user has
authorized it, Rig caches and refreshes credentials independently. No API key,
`OPENAI_BASE_URL`, sidecar, or separate installation is required.

This backend is a ChatGPT/Codex product interface, not the public OpenAI
Platform API, and has no equivalent public compatibility guarantee. The
adapter is therefore explicitly experimental. It currently supports the native
Responses shape; it does not claim a Chat Completions subscription endpoint.
Selecting this named adapter is the runtime opt-in; there is no Cargo feature
that could be mistaken for a security boundary or accidentally skipped by CI.

Important boundaries:

- Never configure Rig's `auth_file` as `~/.codex/auth.json`. Codex and Rig use
  different schemas, and sharing a rotating refresh token between independent
  clients can break either login.
- The convenience connector uses BONE's independent record at
  `~/.config/bone/chatgpt-subscription/auth.json` on Unix (respecting
  `XDG_CONFIG_HOME`). It requires an absolute existing config root, creates the
  app directories as `0700`, creates token/lock files as `0600`, uses
  `O_NOFOLLOW`, and rejects symbolic links, foreign-owned files, and hard
  links. The guarded convenience connector is temporarily unavailable on
  Windows until equivalent ACL and reparse-point checks exist. An application
  can supply a different app-owned path through a configured Rig client and
  `chatgpt_subscription::from_client`.
- Interactive device login is suitable for local CLI or desktop use. A server
  should construct the Rig client with `allow_device_flow(false)` so an
  ordinary request cannot wait for unattended login.
- `connect` completes authorization before it returns, keeping device login out
  of ordinary completion requests. Reuse the returned endpoint: BONE keeps an
  exclusive OS file lock for its entire endpoint/model lifetime, so a second
  process fails safely instead of racing a rotating refresh token. A custom
  client passed to `from_client` owns its own locking policy.
- After explicit authorization, `connect` rebuilds the endpoint client with
  interactive device flow disabled. A rejected refresh therefore returns a
  redacted reconnect error instead of printing a new code or waiting inside an
  ordinary completion. Generic provider setup errors are likewise redacted at
  the guarded model boundary.
- Rig drops unsupported backend fields, including `max_output_tokens` and
  `temperature`; it forces `stream: true`, `store: false`, and requests
  replayable encrypted reasoning state. Do not treat `max_tokens` as a hard
  subscription budget boundary.
- Native OAuth is unavailable on WASM. Subscription live tests must remain
  manual and must not put a personal refresh token in hosted CI.

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

Ordinary protocol constructors do not read environment variables or persist
credentials; they receive resolved values explicitly. An explicitly selected
OAuth service adapter may maintain the independent token cache documented by
that adapter. Local construction failures use `ConfigError`; the explicit
subscription handshake returns a redacted `ConnectError`; request-time network,
protocol, and model failures remain Rig `CompletionError` values.

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

The experimental ChatGPT subscription probe needs only a model identifier.
Its first run performs device login; later runs reuse the independent cache:

```sh
export BONE_CHATGPT_MODEL='a-model-available-to-your-subscription'

cargo run -p bone-provider --example chatgpt_subscription_probe -- text
cargo run -p bone-provider --example chatgpt_subscription_probe -- tool
```

Tool mode only displays the requested call. It does not execute the tool.
