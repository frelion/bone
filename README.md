# BONE

BONE is an event-driven agent runtime written in Rust. `bone-agent` owns a
synchronous session kernel and a shared asynchronous execution path for models
and tools. `bone` connects it to a real model and workspace tools; `bone-llm`
also provides a terminal product for talking directly to a model.

## Workspace

```text
bone-llm binary ──► bone-llm library ──► rig-core

bone-cli ────┬─runs────────► bone-agent (kernel, runtime, ports)
             ├─adapts──────► bone-llm ──► rig-core
             ├─configures──► bone-config
             └─adapts──────► bone-tools ──┬──► bone-llm protocol values
                                         └──► bone-config
```

The five workspace crates are:

- `bone-agent`: the session kernel, model/tool ports, and unified job runtime.
- `bone-llm`: the unified model library, service adapters, and official
  direct-model terminal product. It owns model-facing tool definitions, calls,
  outputs, and provider translation.
- `bone-tools`: the native typed `Tool` interface and built-in implementations.
- `bone-config`: typed, non-secret configuration storage.
- `bone-cli`: the model/tool adapters and responsive terminal input loop.

The `bone-llm` library receives endpoint settings, credentials, and storage
roots through its constructors; it does not depend on `bone-config`. Its binary
is a small composition root that currently selects the ChatGPT subscription
adapter, reads its model ID from `BONE_MODEL`, and explicitly supplies BONE's
conventional local credential root. Agent integration is assembled in
`bone-cli`; `bone-agent` has no dependency on a model provider or native tools.

In production code, Rig is confined to `bone-llm`. Agent and built-in-tool APIs
use BONE types: `bone-agent` owns execution, while `bone-tools` supplies
concrete implementations through the CLI adapters.

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

Create the system configuration at `$XDG_CONFIG_HOME/bone/config.json`, or
`$HOME/.config/bone/config.json` when `XDG_CONFIG_HOME` is unset. Start from
[config.example.json](crates/bone-cli/config.example.json) and replace both
model IDs with models available to your subscription. `coordinator` is a
system setting; `default_solver` supplies the task's default model. They may
use the same model.

The solver owns reasoning, tool selection, and final answers. The coordinator
only interprets user interruptions while a solver decision is outstanding;
uninterrupted work makes zero coordinator calls. Both use the same job runtime.

```sh
cargo run -p bone-cli -- "Read Cargo.toml and list the workspace crates"
```

Select a different solver for one session:

```sh
cargo run -p bone-cli -- --model '<solver model>' "Investigate this design"
```

`--model` takes precedence over `BONE_MODEL`, which takes precedence over the
system's default solver. These overrides do not affect the coordinator or
write back to the system configuration. Each purpose independently supports
an optional reasoning `effort` and a `timeout_seconds` value (default: 120).
Use `BONE_CONFIG` for another absolute system configuration path.
Configuration is read when the session starts; see the
[crate guide](crates/bone-agent/README.md#模型配置的归属) for the full contract.

Run without a message for an interactive prompt. Input remains available while
background jobs run; `/stop` stops autonomous work and `/exit` shuts down.
The deterministic core example needs no model credentials:

```sh
cargo run -p bone-agent --example interleaving
cargo test -p bone-agent
```

Use `cargo run -p bone-agent --example walkthrough` to inspect each synchronous
kernel transition, or CLI `--events session.jsonl` to observe a real session.

Both terminal products currently use
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
