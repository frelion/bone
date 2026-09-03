# BONE

BONE is a small, action-oriented agent runtime written in Rust. Its current
runnable slice connects one conversational Agent to a ChatGPT subscription and
workspace-bound read-only tools.

## Workspace

```text
bone-cli
├──► bone-agent ──► bone-model ──► rig-core
├──► bone-model ─────────────────► rig-core
└──► bone-tools ──┬──────────────► rig-core
                  └──────────────► bone-config
```

`bone-cli` is the composition root. The five workspace crates are:

- `bone-agent`: Agent, Action, Turn, and tool-execution semantics.
- `bone-model`: the unified model boundary and service adapters.
- `bone-tools`: provider-independent local tools.
- `bone-config`: typed, non-secret configuration storage.
- `bone-cli`: the runnable assembly of a model, Agent, and tools.

## Run

```sh
BONE_MODEL='<model available to your subscription>' \
  cargo run -p bone-cli -- "Read Cargo.toml and list the workspace crates"
```

Run without a message for an interactive prompt. The first run may request
ChatGPT device login. Run the test suite with `cargo test --workspace`.

Start with the [Agent](docs/agent.md), [Model API](docs/model-api.md),
[tools](docs/tools.md), and [configuration](docs/configuration.md) documents.
Provider contract and live-test guidance live in
[provider testing](docs/provider-testing.md).

The current implementation lives in [`crates/`](crates/). [`legacy/`](legacy/)
is the historical implementation, not the active architecture.
[`third_party/`](third_party/) contains the pinned Rig patch used by this
workspace.
