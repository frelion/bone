# Configuration and local storage

`bone-app` is currently a headless Rust library. A frontend configures it through
typed `App` methods; there is no user-editable configuration file, TUI command,
or `bone` binary in this crate.

## App-owned data

The host supplies one absolute data directory:

```rust,ignore
let app = bone_app::App::open(bone_app::AppOptions::new(data_dir)).await?;
```

The App creates `<data_dir>/bone.sqlite3` and a private `leases/` directory.
SQLite contains:

- Workspace identities and canonical roots;
- Session metadata, drafts, inputs, request-idempotency records, and history;
- User, Workspace, and Session configuration;
- non-secret provider profiles and frozen Runtime configurations;
- Agent records and external-write attempts.

The private `storage` module owns SQLite documents, journals, transactions,
schema validation, and OS file leases. These types are not part of the public
App API. Existing unsafe permissions, corruption, or a future schema version
produce an error; the App never resets the database automatically.

New records use the `app.v2` namespace. There is no automatic import from the
removed App/TUI model, and old records are not overwritten.

## Profiles and credentials

A `Profile` contains only a stable `ProfileId`, display label, and
`bone_llm::EndpointConfig`. The built-in `chatgpt` profile uses the ChatGPT
subscription protocol. Other profiles can select OpenAI Responses, OpenAI Chat
Completions, or Anthropic Messages, with an optional validated HTTPS-compatible
base URL.

```rust,ignore
let profile = Profile::new(
    ProfileId::new("work-openai")?,
    "Work OpenAI",
    EndpointConfig::OpenAiResponses { base_url: None },
)?;
app.save_profile(profile).await?;
app.set_api_key(ProfileId::new("work-openai")?, ApiKey::new(key)?).await?;
```

API keys live in the operating-system credential manager. Their slots are
bound to both profile ID and endpoint identity, so changing an endpoint cannot
silently redirect an existing key. API keys have no `Debug` or `Display`
implementation and never enter SQLite or Session history.

ChatGPT OAuth remains in the credential cache owned by its provider adapter.
`App::login` returns a short-lived `LoginAttempt` whose watch state exposes the
device code, success, failure, or cancellation. Normal Runtime startup only
uses cached authorization and reports `AppProblem::LoginRequired(profile)`;
it never starts interactive login on its own. `logout` refuses while a live
Runtime still holds that connection.

## Typed configuration

There are three scopes:

```text
Session override > Workspace override > User setting
```

`RuntimeOverrides` has four fields:

- Worker model selection;
- optional Coordinator model selection;
- `bone_agent::AgentLimits`;
- `ToolSettings` (`ReadOnly` or `WorkspaceWrite` plus `ToolLimits`).

If no scope selects a Coordinator, it follows the resolved Worker. If no scope
selects a Worker, `resolved_config` returns `ConfigProblem::NeedsModel`; opening
the App, Workspace, Session, history, and draft still works.

`App::update_config(scope, change)` changes exactly one typed field. The storage
transaction reads the latest value and patches that field, so concurrent
changes to different fields do not overwrite each other. Passing `None` clears
an override; at User scope, limits and tools return to their defaults.

```rust,ignore
app.update_config(
    ConfigScope::Workspace(workspace.id),
    ConfigChange::Worker(Some(ModelSelection::new(profile_id, "model-id")?)),
).await?;

app.update_config(
    ConfigScope::Session(session.id()),
    ConfigChange::Limits(Some(AgentLimits {
        tool_timeout: Duration::from_secs(601),
        ..AgentLimits::default()
    })),
).await?;

app.update_config(
    ConfigScope::Session(session.id()),
    ConfigChange::Tools(Some(ToolSettings {
        mode: ToolMode::WorkspaceWrite,
        limits: ToolLimits::default(),
    })),
).await?;
```

`resolved_config(session)` returns both the desired configuration and the
configuration frozen into the live Runtime, if one exists. Saving new settings
never changes a running Agent. Close that Runtime and retry/submit work to use
the new settings.

Configuration validation also preserves the persistence boundary: model
context, individual model-originated Agent items, Agent tool results, and Patch
input are at most 1 MiB. Each raw tool output stream is at most 512 KiB, so two
Bash streams still fit in an 8 MiB journal/document after worst-case JSON
escaping. The Agent byte values are acceptance ceilings for original untrusted
payloads. Oversized values become fixed diagnostics; because limits need only
be positive, those diagnostics are not promised to fit an arbitrarily tiny
configured value, but their size is fixed and remains far below the App
persistence ceiling. When `ToolMode::WorkspaceWrite` is enabled, Agent tool
timeout must exceed the maximum Bash timeout so the tool has time to report and
persist its cleanup result. Read-only mode does not install Bash and does not
impose that cross-setting relation.

## Session durability and concurrency

`submit` returns success only after the input, its `RequestId` index, and the
`InputSubmitted` fact commit in one transaction. Reusing the same RequestId and
content returns the original receipt; changing the content returns
`RequestConflict`. Text input is limited to 1 MiB before any durable record is
written, leaving room for the Agent record envelope and JSON escaping.

One App serializes its short SQLite write transactions through one writer
connection, while WAL keeps reads available. Ordinary independently opened
connections use a bounded busy timeout. Each open Session also holds a
cross-process OS lease, so two App processes cannot execute the same Session.
Writes from different Sessions in one Workspace share an in-process mutex.
Cross-process serialization of different Sessions writing the same Workspace
is deliberately not guaranteed in this version.

An external write records `Pending` before execution. It remains visible and
blocks later Workspace writes until a matching Agent record durably
acknowledges the result, or a host explicitly resolves it after a crash. The
actual write task retains the Session lease even if Agent shutdown reaches its
grace deadline, preventing another App from reopening that Session while the
old write can still mutate files.

On restart, a stale Runtime is closed in storage and its nonterminal delivered
inputs become `Interrupted`. Inputs proven never to have been delivered remain
`Queued`; external writes are never replayed automatically. When a new Runtime
starts, App currently scans the Session journal linearly from its beginning and
selects bounded recent public events for `BootstrapContext`; there is no
background-history tail index or summary service yet.

The model can continue through durable history with `session_history`, one
stored position at a time after its cursor. If one public event alone exceeds
the Runtime's tool-output budget, and that budget can hold the fixed omission
envelope, the call still returns a successful page with an `omitted` marker and
advances `next_cursor` past that event. It never truncates the stored fact. An
absurdly small budget that cannot hold even the fixed envelope falls back to
the Agent's fixed-diagnostic rule.

See [bone-app architecture](bone-app-design.md) for the complete App/Session
contract.
