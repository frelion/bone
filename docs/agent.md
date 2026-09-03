# Agent core

`bone-agent` gives the user one conversational `Agent`. The user sends
messages; the Agent decides whether to reply or create Actions. Callers cannot
construct or submit Actions directly.

```text
User message
    ↓
Agent decision
    ├── final reply
    └── start Action(s)
            └── Turn(s)
                    └── optional tool calls and results
            ↓
        ActionOutcome
            └────────→ next Agent decision
```

An **Action** is one semantic piece of work selected by the Agent. It may be
pure reasoning or tool-backed work, and it owns an isolated context plus one or
more Turns. A **Turn** is one model decision and the complete tool batch caused
by that decision. A tool failure is an observation for the next Turn; it does
not automatically fail the Action.

The Agent's private `start_action` command is only how a model asks the runtime
to create an Action. It is not the Action itself and never appears as one of an
Action's tools.

## API

```rust,ignore
let environment = ToolEnvironment::new(workspace)?;
let mut agent = Agent::new(model)
    .instructions("Inspect facts before making claims.")
    .tool(environment.read())?
    .tool(environment.glob())?
    .tool(environment.grep())?;

let reply = agent.chat("Read Cargo.toml and explain the workspace").await?;
println!("{}", reply.text());

for action in reply.actions() {
    println!("{}: {} turn(s)", action.intent(), action.turns().len());
}
```

`Agent::chat` keeps provider-valid message history. It commits the new history
only after producing a final reply; cancelling or failing a response leaves the
previous history unchanged. `AgentReply` contains the final user-facing text
and the complete Actions that informed it.

## Scheduling

Action state is derived from its trace:

| Facts | State |
| --- | --- |
| Terminal outcome exists | `Finished` |
| A tool result is unresolved | `Waiting` |
| Otherwise | `Ready` |

The runtime follows four rules:

1. One Agent makes at most one model request at a time.
2. Tool calls from one Turn start concurrently.
3. An Action starts its next Turn only after that Turn's complete tool batch settles.
4. While Action A waits for a long tool, another ready Action B may advance.

One Agent decision can create several independent Actions. The first version
waits for that whole Action batch before asking the Agent to choose more work.
This keeps provider tool-result ordering unambiguous without introducing a
background actor or Exchange lifecycle.

Finite decision, Turn, tool-fan-out, and model-request limits prevent runaway
model loops. Every tool future is also wrapped in a runtime timeout (fifteen
minutes by default, configurable with `Agent::tool_timeout`). A tool may enforce
a shorter domain-specific deadline. Expiry drops a cooperative future and
becomes an ordinary failed tool observation, so the Action can recover or
finish. Tool implementations must not block their executor thread and must own
cancellation of detached work.

The history commit rule applies only to the model transcript. Cancelling a
`chat` does not undo external effects already performed by a tool; effectful
tools must therefore provide their own cancellation and retry guarantees. In
particular, dropping a tool future must not leave effects or cleanup that can
conflict with a later Turn. The first runnable slice avoids that boundary by
exposing only read-only tools; write tools are deliberately not wired in yet.

## Runnable slice

The `bone` binary connects the same core through the unified
`bone_provider::Model` interface to a ChatGPT subscription and the real
workspace-bound `read`, `glob`, and `grep` tools:

```text
BONE_MODEL='<model available to your subscription>' \
  cargo run -p bone-cli -- "Read Cargo.toml and list the workspace crates"
```

Run it without a message for a small interactive prompt. No API key is needed.
The first run may display a ChatGPT device-login URL and code; later runs reuse
BONE's independent credential cache. This managed connector is experimental
and currently requires Unix. It writes first-run device codes to stderr, so do
not redirect authentication output to persistent logs. The CLI selects the
service and renders login prompts; credential lifecycle and protocol
translation end at `bone-provider`, while `bone-agent` receives only the
selected `Model`. Run the CLI from the intended workspace: tools are read-only
in this slice, but content they read is sent to the model.

This crate intentionally does not yet model Exchange, Conversation, Task,
planning, persistence, teams, streaming UI, or a long-lived mailbox actor.
