# Agent core

`bone-agent` executes independent Actions. It deliberately stops at that
boundary: Exchange, Conversation, planning, teams, persistence, and UI events
belong above this crate.

```text
Agent
├── Action A
│   ├── Turn 1 ── tool A ─┐
│   │                     ├── next Turn only after the whole batch settles
│   │          ── tool B ─┘
│   └── Turn 2 ── final output
└── Action B
    └── Turn 1 ── final output
```

An **Action** is one independently resumable piece of work with its own model
transcript. A **Turn** is one model decision plus the tool batch caused by that
decision. Tool failure is recorded as an observation for the next Turn; it
does not automatically fail the Action.

Action state is derived rather than stored:

| Facts | State |
| --- | --- |
| Terminal outcome exists | `Finished` |
| A tool result is unresolved | `Waiting` |
| Otherwise | `Ready` |

The scheduler has four rules:

1. It makes at most one model request at a time.
2. All tool calls from one Turn start concurrently.
3. The Action resumes only after every tool in that Turn has settled.
4. While one Action waits for tools, another ready Action may advance.

This is enough to prevent a long command in Action A from occupying the
Agent's decision loop: Action B can continue. A tool that never returns still
keeps A in `Waiting`; this first API waits for all supplied Actions before
returning and does not yet expose a live handle for adding, observing, or
cancelling one Action independently. Dropping the whole `run` future cancels
its in-flight tool futures.

## API

```rust,ignore
let agent = Agent::new(model)
    .instructions("Work carefully and report verified results.")
    .tool(read)?
    .tool(bash)?
    .max_turns(24)?;

let action = agent.act("Find and explain the failing test").await;
match action.outcome() {
    Some(ActionOutcome::Completed { output }) => println!("{output}"),
    Some(ActionOutcome::Failed(error)) => eprintln!("{error}"),
    None => unreachable!("act runs an Action to a terminal outcome"),
}

let actions = agent
    .run([
        Action::new("Inspect the provider implementation"),
        Action::new("Run the focused integration tests"),
    ])
    .await;
```

`run` preserves input order and failure isolation. Each returned Action keeps
its complete Turn and tool trace, including work that occurred before a later
model failure. Operator-facing provider diagnostics remain available as the
error source, while the normal error display does not expose raw provider
response bodies.

## Safety bounds

The defaults are intentionally finite:

- 32 model Turns per Action;
- 16 tool calls per Turn;
- 120 seconds per model request.

If a response is truncated or exceeds the tool-call limit, the complete batch
is marked skipped before any tool starts. Tool duration is left to the tool's
own policy because some legitimate commands are long-running; its waiting
future does not prevent other Actions from advancing.
