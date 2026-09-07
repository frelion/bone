# BONE

BONE is an event-driven Agent written in Rust. `bone-agent` provides the
execution API, model/tool adapters, and session runtime. `bone-app` provides
the terminal frontend, durable Workspace/Session state, and the `bone`
executable.

## Workspace

```text
bone-app ──────────┬─► bone-agent ──┬─► bone-llm ──► rig-core
                   │                 └─► bone-tools ──► bone-llm
                   └─► bone-config

bone-config remains the typed, atomic persistence primitive used by the
application and Agent layers.
```

- `bone-app`: owns the `bone` binary, full-screen interaction, command parsing,
  event export, product-level workspace bootstrap, and typed settings scopes.
  Its private durable modules own Workspace identities, session metadata,
  drafts, and append-only recovery facts; they never create `.bone/` in a user
  project. It starts without an Agent connection so setup, history, and drafts
  remain available on first run or during repair.
- `bone-agent`: session creation, model/tool integration, synchronous kernel,
  and asynchronous execution.
- `bone-config`: shared typed configuration, snapshots, and atomic persistence.
- `bone-llm`: model protocols and service connections. It is a library.
- `bone-tools`: native workspace tools and their execution limits.

`AgentHost` still connects the model service once and starts isolated Agent
runtimes. A logical session is now separate from an attached runtime: its
identity, title, draft, and journal survive reopening the same launch directory.

## Run

For normal interactive use, start BONE from the directory you want it to work
in. That exact directory is the Workspace boundary; BONE does not silently walk
up to a Git root. On first run it creates its own private user-data/configuration
directories and a draft conversation. You do **not** need to create or edit a
JSON file, and BONE does not put a `.bone` directory into your repository.

```sh
cd ~/code/my-project
bone

# During development from this repository:
cd ~/code/my-project
cargo run --manifest-path /path/to/bone/Cargo.toml -p bone-app --bin bone
```

Choose a model in the TUI; settings are saved immediately:

```text
/model gpt-5.6             current conversation
/model default gpt-5.6     default for this Workspace
/model global gpt-5.6      default for this user
/model inherit             remove this conversation's override
/login                     connect or retry connection
/new  /sessions  /resume   manage conversations in this Workspace
/rename <title>  /archive  organize the current conversation
/status  /workspace        inspect the current state
```

Only a typed, single-line slash command is local. Pasted/multiline text is
always sent as normal model-visible input; use `//text` to intentionally send a
slash-prefixed message. Unknown commands remain local and show suggestions.

The full-screen frontend keeps several conversations visible at once. `Ctrl-N`
creates one; `Ctrl-Left`, `Up`/`Down`, and `Ctrl-Right` operate the session rail;
`Esc` stops current work and `Ctrl-C` exits. Drafts and visible journal history
are durable, while an unexpected stop is recorded as an interruption rather
than silently replayed.

The current Agent core fixes its model adapter when an individual runtime is
created. Therefore a model selection is saved immediately, but an already
attached runtime keeps its pinned model; new conversations and future recreated
runtimes use the new selection. Per-turn hot model application is the next core
runtime milestone and is deliberately not claimed as already implemented.

For one request, pass its text:

```sh
cargo run -p bone-app --bin bone -- "Read Cargo.toml and list the workspace crates"
```

`--model` in interactive mode becomes the first logical session's saved
session-level choice. `BONE_MODEL` is the same convenience input when no flag is
provided. One-shot execution remains a strict automation surface and currently
expects a valid Agent system configuration; complete interactive setup once for
the no-file-editing path. For one-shot execution, `--events session.jsonl`
writes a new file containing the initial snapshot and live kernel events.

The solver owns reasoning, tools, and answers. The coordinator only interprets
interruptions while the solver is busy. Uninterrupted work makes no coordinator
calls. The current application exposes `read`, `glob`, and `grep` tools.

## Develop

Start with [the Agent API](docs/agent.md), the
[crate walkthrough](crates/bone-agent/README.md), and
[shared configuration](docs/configuration.md). The full-screen implementation
is explained in [TUI architecture](docs/tui-architecture.md).

```sh
cargo run -p bone-agent --example walkthrough
cargo run -p bone-agent --example interleaving
cargo test --workspace --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
```

The examples use controlled ports and need no credentials. Model protocol and
live-test details are in [Model API](docs/model-api.md) and
[provider testing](docs/provider-testing.md); native tools are documented in
[tools](docs/tools.md).

The active implementation is in [`crates/`](crates/). [`legacy/`](legacy/) holds
historical code. [`third_party/`](third_party/) contains the pinned Rig patch.

## Product design

The vNext product contract for zero-configuration startup, live TUI settings,
workspace-bound persistent sessions, and slash commands is documented in the
[product requirements](docs/product/tui-workspace-prd.md). Screen behavior,
keyboard interaction, responsive layouts, model selection, setup, and recovery
states are specified in the [TUI interaction design](docs/product/tui-interaction-design.md).
The current implementation boundary and the migration from the existing single
UI writer to strict event/effect/data flow are documented in the
[product runtime architecture](docs/product/tui-runtime-architecture.md).
For a reviewable static visual of the target experience, open the
[high-fidelity TUI design](docs/product/bone-tui-design.html).
