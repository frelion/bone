# BONE

BONE is an event-driven Agent written in Rust. `bone-agent` provides the complete
application API, model/tool adapters, and session runtime. `bone-tui` is the
terminal frontend; the executable is still named `bone`.

## Workspace

```text
bone-tui ──► bone-agent ──► bone-llm ──► rig-core
                       └─► bone-tools

bone-config supplies configuration to all four modules.
```

- `bone-agent`: session creation, model/tool integration, synchronous kernel,
  and asynchronous execution.
- `bone-tui`: terminal input, replies, progress, and event export. Its BONE
  dependencies are only `bone-agent` and `bone-config`.
- `bone-config`: shared typed configuration, snapshots, and atomic persistence.
- `bone-llm`: model protocols and service connections. It is a library.
- `bone-tools`: native workspace tools and their execution limits.

Each module owns its configuration types. Agent creation reads one fresh
snapshot, builds the model and tools, then injects ordinary parameters into
the kernel. Configuration changes affect later sessions; running sessions keep
what they were created with.

## Run

Create `$XDG_CONFIG_HOME/bone/config.json`, or `$HOME/.config/bone/config.json`
when `XDG_CONFIG_HOME` is unset. Start from
[config.example.json](crates/bone-tui/config.example.json) and select models
available to your subscription. Existing files containing only `agent.system`
remain valid. `BONE_CONFIG` can select another absolute configuration path.

```sh
cargo run -p bone-tui
```

The frontend currently uses line-based terminal interaction. Input stays open
while work runs. `/stop` stops autonomous work; `/exit` closes the session.
The application connects to the existing ChatGPT subscription service and
reuses BONE's independent login; initial authorization is displayed in the
terminal when needed.

For one request, pass its text:

```sh
cargo run -p bone-tui -- "Read Cargo.toml and list the workspace crates"
```

Use `--model` to override the solver for this session. It takes precedence over
`BONE_MODEL`, then the configured default. The coordinator remains a system
setting. `--events session.jsonl` writes a new file containing the initial
snapshot and live kernel events.

The solver owns reasoning, tools, and answers. The coordinator only interprets
interruptions while the solver is busy. Uninterrupted work makes no coordinator
calls. The current application exposes `read`, `glob`, and `grep` tools.

## Develop

Start with [the Agent API](docs/agent.md), the
[crate walkthrough](crates/bone-agent/README.md), and
[shared configuration](docs/configuration.md).

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
