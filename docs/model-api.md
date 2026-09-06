# Model API

`bone-llm` is BONE's provider-independent model library. Provider clients and
wire DTOs are implementation details; callers use BONE types from request
construction through response replay.

```text
Protocol                 Endpoint                  Model
wire contract      +     configured service  +     selected model
                         credentials + URL

Request ───────────────► complete ───────────────► Response
        └──────────────► stream   ───────────────► ResponseStream
```

There is one model selection path and two execution modes:

```rust,no_run
use bone_llm::{InputItem, InputSource, Request, protocol::openai_responses};

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
use bone_llm::{InputItem, InputSource};

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
use bone_llm::{InputItem, InputSource, Model, Request};

# async fn example(model: Model) -> Result<(), bone_llm::Error> {
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
use bone_llm::{
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
#     call: &bone_llm::ToolCall,
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

These are model-protocol values, not an execution framework. `bone-llm` never
registers or runs a tool. `bone-agent` adapts built-in tools to its `ToolPort`
and uses these values to advertise tools, dispatch calls, and return results;
`bone-tools` owns the native `Tool` interface and built-in implementations.
Provider and Rig tool types do not cross the `bone-llm` boundary.

`InputItem::tool_result` accepts the complete `ToolCall`, not a loose string
ID, so protocol-specific correlation data cannot be accidentally discarded.
Adjacent tool results are sent as one result batch when the wire requires it.
BONE checks both the public correlation handle and opaque provider call/item
handles against committed history before returning a response, so an Agent
never executes a tool call with reused identity.

## Streaming

Streaming exposes display deltas and one canonical terminal response:

```rust,no_run
use bone_llm::{InputItem, InputSource, Model, Request, StreamEvent};
use futures_util::StreamExt;

# async fn example(model: Model) -> Result<(), bone_llm::Error> {
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
use bone_llm::{Request, protocol::openai_responses};

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
- `chatgpt_subscription::connect` for the experimental ChatGPT subscription
  service.

Compatible base URLs must be absolute HTTP(S) URLs without embedded
credentials or query strings. Authentication and routing configuration are
injected while constructing the endpoint.

For native sessions, `LlmConfig` registers the `llm.system` section with
`bone-config`. Its only setting is optional `credential_root`, an absolute
directory for BONE's independent subscription credentials. An empty section
uses the existing default credential directory. `bone-agent::start` reads this
section from the session's shared configuration snapshot before connecting.

The ChatGPT subscription connector receives an explicit application-owned
credential root. `default_credential_root()` is an opt-in convenience, while
`connect` owns authorization, locking, secure credential storage, and refresh.
The backend does not honor `max_output_tokens` or structured-output schemas,
so BONE rejects those options locally instead of pretending they were applied.
A credential root supports one live subscription connection. Starting another
session with the same root returns `CredentialStoreBusy` until the existing
connection's endpoint and model handles are released.

`bone-agent` composes the models, tools, and runtime. The terminal frontend is
`bone-tui`, which depends on `bone-agent` and `bone-config`; run it with
`cargo run -p bone-tui` after [configuring the agent](configuration.md).
