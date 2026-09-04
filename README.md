# BONE

BONE is a small, action-oriented agent runtime written in Rust. The workspace
currently contains two runnable slices: `bone-llm` talks directly to a model,
while `bone` adds the Agent, Actions, Turns, and workspace-bound read-only
tools.

## Workspace

```text
bone-llm binary ──► bone-llm library ──► rig-core

bone-tools ──┬──implements──► bone-agent::Tool
             └──uses protocol values──► bone-llm
bone-cli ────┬─composes────► bone-agent ──► bone-llm ──► rig-core
             ├─selects─────► bone-llm
             └─constructs──► bone-tools ──► bone-config
```

The five workspace crates are:

- `bone-agent`: Agent, Action, Turn, and the single BONE tool-execution
  interface, including registration and scheduling.
- `bone-llm`: the unified model library, service adapters, and official
  direct-model terminal product. It owns model-facing tool definitions, calls,
  outputs, and provider translation.
- `bone-tools`: provider-independent built-in implementations of
  `bone-agent::Tool`.
- `bone-config`: typed, non-secret configuration storage.
- `bone-cli`: the runnable assembly of a model, Agent, and tools.

The `bone-llm` library receives endpoint settings, credentials, and storage
roots through its constructors; it does not depend on `bone-config`. Its binary
is a small composition root that currently selects the ChatGPT subscription
adapter, reads its model ID from `BONE_MODEL`, and explicitly supplies BONE's
conventional local credential root. The intended full-product composition is
`bone-config → bone-llm → Agent`.

In production code, Rig is confined to `bone-llm`. Agent and built-in-tool APIs
use BONE types: `bone-agent` owns execution, while `bone-tools` supplies
concrete implementations.

## Talk directly to a model

```sh
BONE_MODEL='<model available to your subscription>' cargo run -p bone-llm
```

The official `bone-llm` binary streams a multi-turn conversation without an
Agent or tools. Use `/clear` to reset its in-memory history and `/exit` to quit,
or pass one message for a one-shot request:

```sh
BONE_MODEL='<model available to your subscription>' \
  cargo run -p bone-llm -- "Reply with exactly: ok"
```

## Run the Agent

```sh
BONE_MODEL='<model available to your subscription>' \
  cargo run -p bone-cli -- "Read Cargo.toml and list the workspace crates"
```

Run without a message for an interactive prompt. Both products currently use
the experimental ChatGPT subscription slice; the first run may request device
login and later runs reuse BONE's independent credential cache. Run the full
test suite with `cargo test --workspace --all-features`.

Start with the [Agent](docs/agent.md), [Model API](docs/model-api.md),
[tools](docs/tools.md), and [configuration](docs/configuration.md) documents.
Provider contract and live-test guidance live in
[provider testing](docs/provider-testing.md).

The current implementation lives in [`crates/`](crates/). [`legacy/`](legacy/)
is the historical implementation, not the active architecture.
[`third_party/`](third_party/) contains the pinned Rig patch used by this
workspace.
