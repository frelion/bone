# Provider API

`bone-provider` is intentionally thin. Rig owns provider clients, normalized
messages, requests, responses, tool calls, provider state, and streaming. BONE
does not mirror those types.

```text
future Turn / Action  <->  Rig types  <->  provider
```

The only added abstraction is `Model`: a cloneable handle that erases a
concrete Rig model type. This is needed because Rig's `CompletionModel` trait
is not object-safe, while the runtime will select models dynamically.

## OpenAI Responses protocol

```rust,no_run
use bone_provider::{
    openai::{OpenAi, Reasoning, ReasoningEffort, reasoning_params},
    rig::message::Message,
};

# async fn example(api_key: String) -> Result<(), Box<dyn std::error::Error>> {
let model = OpenAi::new(api_key)?.model("gpt-5.6-luna")?;
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

- Credentials are supplied explicitly and are moved into Rig's authorization
  header. BONE does not persist them.
- `OpenAi::compatible` accepts a different Responses-compatible API root. The
  endpoint may be OpenAI, a gateway such as New API, or another vendor such as
  DeepSeek; BONE does not add vendor-shaped wrapper types.
- `OpenAi::from_client` remains the escape hatch for custom headers or a custom
  Rig HTTP transport.
- `reasoning_params` only encodes Rig's typed OpenAI reasoning controls into
  the generic Rig request field. Rig owns the actual Responses conversion,
  including encrypted reasoning state.
- Provider call failures remain Rig `CompletionError` values. Sanitization and
  user-facing presentation belong at the future runtime/GUI boundary.

The real API smoke test is ignored by default:

```sh
export OPENAI_API_KEY='...'
export BONE_OPENAI_MODEL='gpt-5.6-luna'
cargo test -p bone-provider --test openai_live -- --ignored
```

For a compatible endpoint, set its full API root. It must be the prefix to
which `/responses` is appended:

```rust,no_run
use bone_provider::openai::OpenAi;

# fn example(api_key: String) -> Result<(), Box<dyn std::error::Error>> {
let model = OpenAi::compatible(api_key, "https://gateway.example/v1")?
    .model("vendor-model")?;
# Ok(())
# }
```

Compatibility is about the wire endpoint, not the company name. This adapter
requires the OpenAI Responses shape; an endpoint that implements only
`/chat/completions` is not silently treated as equivalent.

## Inspect the live boundary

`openai_probe` makes the provider boundary visible without adding an agent
runtime. It prints the normalized Rig request, each normalized stream event,
and Rig's final aggregated assistant choice.

```sh
export OPENAI_API_KEY='...'
export BONE_OPENAI_MODEL='...'
# Optional for DeepSeek, New API, or another compatible endpoint:
export OPENAI_BASE_URL='https://gateway.example/v1'

# Observe reasoning and text events.
cargo run -p bone-provider --example openai_probe -- text

# Force a tool call. The probe displays it but deliberately does not execute it.
cargo run -p bone-provider --example openai_probe -- tool
```

The tool mode defines a harmless fictional `inspect_path` function so the
request and response shapes can be inspected before BONE has a tool runtime.
No filesystem access occurs.
