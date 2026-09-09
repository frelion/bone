# Model API

> The `bone_adapters::llm` contract and examples remain current. Product assembly notes
> from the former TUI application are superseded by the
> [headless App architecture](bone-app-design.md).

`bone_adapters::llm` is BONE's provider-independent model module. Provider
clients and wire DTOs are implementation details; callers use BONE types from
request construction through response replay.

```text
Protocol                 Endpoint                  Model
wire contract      +     configured service  +     selected model
                         credentials + URL

Request ───────────────► complete ───────────────► Response
        └──────────────► stream   ───────────────► ResponseStream
```

There is one model selection path and two execution modes:

```rust,no_run
use bone_adapters::llm::{InputItem, InputSource, Request, protocol::openai_responses};

# async fn example(api_key: String) -> Result<(), Box<dyn std::error::Error>> {
let endpoint = openai_responses::official("openai-primary", api_key)?;
let model = endpoint.model("your-model")?;

let response = model
    .complete(Request::new([InputItem::external(
        InputSource::User,
        "Hello",
    )]))
    .await?;

println!("{}", response.text().unwrap_or_default());
# Ok(())
# }
```

There is deliberately no `RequestBuilder`, `request.send()`, request-level
model override, public provider client, or generic JSON parameter bag.

## Structured context

A request has four independent concerns:

```text
Request
├── instructions       high-authority policy for this call
├── input[]             ordered committed context and current input
├── tools               callable interfaces
└── controls            output format, token limit, typed protocol options
```

`instructions` is not a history message. BONE injects it on every call and the
protocol adapter maps it to the wire's correct system/developer mechanism.

`input` is one ordered sequence; there is no separate `history` and `prompt`.
An item is one of:

- external input from the human user or a named participant;
- an assistant example;
- an opaque replay item produced by `Response::into_item()`;
- a result tied to one exact `ToolCall`.

Roles are relative to the model currently being called. The model's own prior
response is assistant history. Another agent's output is named external input,
not assistant history:

```rust
use bone_adapters::llm::{InputItem, InputSource};

let human = InputItem::external(InputSource::User, "Please review this.");
let researcher = InputItem::external(
    InputSource::Named("researcher".to_owned()),
    "I found three relevant files.",
);
```

Named sources are attribution only. They never increase authority.

## Multi-turn replay

Never reconstruct assistant history from display text or response IDs. The
response owns the exact replayable state:

```rust,no_run
use bone_adapters::llm::{InputItem, InputSource, Model, Request};

# async fn example(model: Model) -> Result<(), bone_adapters::llm::Error> {
let user = InputItem::external(InputSource::User, "Remember the number 7.");
let first = model.complete(Request::new([user.clone()])).await?;
let assistant = first
    .into_item()
    .expect("a non-empty response has a replay item");

let second = model
    .complete(Request::new([
        user,
        assistant,
        InputItem::external(InputSource::User, "What was the number?"),
    ]))
    .await?;
# Ok(())
# }
```

The opaque item preserves tool correlation IDs, reasoning identifiers,
encrypted reasoning, and provider signatures. `into_item()` returns `None`
when a provider legally finishes with no assistant content; BONE never
fabricates an invalid empty history message.

The current public output surface is intentionally text, tool calls, and safe
reasoning summaries. If a completion provider returns an image, BONE reports a
protocol error instead of silently omitting or textifying it.

Replay state is bound to the endpoint, protocol, and requested model that
produced it. To pass a result between agents or models, send it as named
external input.

## Tools

Definitions and results are BONE types:

```rust,no_run
use bone_adapters::llm::{
    InputItem, InputSource, Request, ToolChoice, ToolDefinition, ToolOutput,
};

let inspect = ToolDefinition::new(
    "inspect_path",
    "Inspect one filesystem path.",
    serde_json::json!({
        "type": "object",
        "properties": { "path": { "type": "string" } },
        "required": ["path"],
        "additionalProperties": false
    }),
);

let request = Request::new([InputItem::external(
    InputSource::User,
    "Inspect /tmp/bone",
)])
.tools([inspect])
.tool_choice(ToolChoice::Specific(vec!["inspect_path".to_owned()]));

# fn next_request(
#     original_user: InputItem,
#     assistant: InputItem,
#     call: &bone_adapters::llm::ToolCall,
# ) -> Request {
Request::new([
    original_user,
    assistant,
    InputItem::tool_result(
        call,
        ToolOutput::json(serde_json::json!({ "kind": "directory" })),
    ),
])
# }
```

These are model-protocol values, not an execution framework. The
`bone_adapters::llm` module never registers or runs a tool. The sibling
`bone_adapters::tools` module owns the native `Tool` interface and built-in
implementations, while the crate's Core adapter maps them to `ToolPort`.
Provider and Rig tool types do not cross the `bone-adapters` boundary.

`InputItem::tool_result` accepts the complete `ToolCall`, not a loose string
ID, so protocol-specific correlation data cannot be accidentally discarded.
Adjacent tool results are sent as one result batch when the wire requires it.
BONE checks both the public correlation handle and opaque provider call/item
handles against committed history before returning a response, so an Agent
never executes a tool call with reused identity.

## Streaming

Streaming exposes display deltas and one canonical terminal response:

```rust,no_run
use bone_adapters::llm::{InputItem, InputSource, Model, Request, StreamEvent};
use futures_util::StreamExt;

# async fn example(model: Model) -> Result<(), bone_adapters::llm::Error> {
let mut stream = model
    .stream(Request::new([InputItem::external(
        InputSource::User,
        "Hello",
    )]))
    .await?;

let mut completed = None;
while let Some(event) = stream.next().await {
    match event? {
        StreamEvent::TextDelta(text) => print!("{text}"),
        StreamEvent::Completed(response) => completed = Some(response),
        StreamEvent::ToolCallDelta { .. } => {}
        _ => {}
    }
}
# Ok(())
# }
```

A fully consumed stream terminates with exactly one `Completed(Response)` or
one `Error`. Text and tool-call fields may appear as deltas for display, but a
complete tool call has exactly one trusted source: the terminal `Response`.
`Completed` is emitted only after the provider stream is drained and its final
response is fully aggregated. EOF without a genuine provider terminal is
`ErrorKind::IncompleteStream`; partial output is never silently committed as a
successful turn. The first provider stream error is terminal: BONE emits it
immediately and drops the remaining stream instead of waiting on a failed
connection.

Unary and streaming calls therefore converge on the same `Response` and the
same `Response::into_item()` continuation path.

## Controls and protocol options

Portable controls stay on `Request`:

- `.max_output_tokens(n)`
- `.output(OutputFormat::Text | OutputFormat::JsonSchema(...))`
- `.tools(...)` and `.tool_choice(...)`

Protocol-only controls live in that protocol's typed `Options`:

```rust
use bone_adapters::llm::{Request, protocol::openai_responses};

# fn configure(request: Request) -> Request {
request.options(
    openai_responses::Options::new().reasoning(
        openai_responses::Reasoning::new()
            .effort(openai_responses::ReasoningEffort::High)
            .summary(openai_responses::ReasoningSummary::Concise),
    ),
)
# }
```

`temperature` is intentionally not a universal BONE control. It is not a
reliable freedom across modern reasoning models or BONE's supported services.
A control belongs in a protocol-specific typed option only when the adapter can
faithfully send it. An accepted option is either honored or rejected before
network I/O; it is never silently dropped.

For persistence outside this crate, use `EndpointConfig` and `ModelOptions`.
They contain protocol/base-URL and protocol-scoped request controls only—not
an endpoint ID, API key, OAuth cache, or storage handle. App code validates a
`ModelOptions` value against the selected endpoint protocol before constructing
an in-memory `Endpoint` and applying it through a configured model.

Automatic tool selection is expressed only by omitting `tool_choice`; every
explicit choice requires at least one tool definition. OpenAI Chat Completions
cannot enforce a JSON schema on an initial request that also advertises tools,
so BONE rejects that combination locally. The same schema is accepted after a
tool result, when the protocol can actually send it.

## Endpoints

Supported public endpoint constructors are:

- `openai_responses::official` / `compatible`;
- `openai_chat_completions::official` / `compatible`;
- `anthropic_messages::official` / `compatible`;
- `chatgpt_subscription::connect` and `connect_cached` for interactive and
  non-interactive ChatGPT subscription connection.

Compatible base URLs must be absolute HTTP(S) URLs without embedded
credentials or query strings. Authentication and routing configuration are
injected while constructing the endpoint.

`bone-adapters` permits HTTP compatible URLs for controlled embedding and test
environments. The BONE App profile layer is stricter: it requires HTTPS before
it will retrieve and send an API key.

The ChatGPT subscription connector accepts a narrow OAuth-cache capability,
acquired by the product composition root:

```rust,ignore
let credentials = ChatGptCredentials::default_for_current_user()?;
let auth = credentials.acquire()?;
let endpoint = chatgpt_subscription::connect("bone-app", auth, show_device_code).await?;
```

The lease holds an exclusive, verified `auth.json` path for Rig's OAuth cache.
Rig remains the sole owner of that file's JSON schema and token refresh
lifecycle. `bone-adapters` never discovers a credential root, reads OAuth bytes, or
deletes the file. The resulting `Endpoint` and every selected `Model` retain
the capability. The caller decides how to share or serialize an acquired
lease; `bone-adapters` never discovers or manages the credential location itself.

The backend does not honor `max_output_tokens` or structured-output schemas,
so BONE rejects those options locally instead of pretending they were applied.

`bone-app` is the composition root: it opens its private storage, resolves
typed User/Workspace/Session settings into a non-secret `RuntimeConfig`,
acquires provider credentials, builds a `ModelAdapter`, assembles tools and
history background, then starts `Agent::with_ports_and_background`.
`bone-core` performs no storage or configuration read while the Runtime is
working. See the [App architecture](bone-app-design.md).
