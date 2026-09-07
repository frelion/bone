# BONE

BONE is a Rust coding agent with a full-screen terminal interface. Start it in
the directory where you want to work: that exact directory is the Workspace.
BONE does not walk up to a Git root and never creates a `.bone` directory in
the project.

The product is designed so normal users never need to locate or edit a
configuration file. The terminal is the settings surface, and changes are
saved immediately.

## Architecture

```text
bone-app ──────────┬─► bone-agent ──┬─► bone-llm ──► rig-core
                   │                 └─► bone-tools ──► bone-llm
                   └─► bone-store ◄──────────────────────┘
```

- `bone-app` is the composition root: the `bone` binary, Workspace/Session
  domain, settings policy, and the terminal UI.
- `bone-store` is BONE's concrete local persistence service. It owns typed
  SQLite documents, journals, and runtime leases; it is not a generic KV API.
- `bone-agent` receives an already-resolved, immutable runtime configuration.
  It never opens user storage or reads settings while a runtime is working.
- `bone-llm` owns protocol adapters. Its ChatGPT subscription adapter receives
  a provider-auth lease rather than a credential directory.
- `bone-tools` provides workspace-local tools and their validated limits.

The TUI has a unidirectional presentation flow:

```text
terminal / runtime event → App::reduce → typed effect → result AppEvent → App::reduce → render
```

Reducers do not access storage or the network. Durable writes happen in the
effect layer, and rendering is a pure projection of `App` state.

## Run

For interactive use, launch BONE from the desired Workspace:

```sh
cd ~/code/my-project
bone

# During development from this repository:
cd ~/code/my-project
cargo run --manifest-path /path/to/bone/Cargo.toml -p bone-app --bin bone
```

The first launch creates a Workspace record, a draft Session, and default
global settings. It deliberately does **not** guess a model or start a login.
You can browse Sessions and edit a draft while the UI shows the normal
`NeedsModel` state.

Choose a model directly in the TUI:

```text
/model gpt-5.6             set this Session's solver override
/model default gpt-5.6     set this Workspace's default solver
/model global gpt-5.6      set the user's default solver
/model inherit             remove this Session override
/login                     connect or retry ChatGPT subscription login
/logout                    remove the local login cache when no runtime owns it
/new  /sessions  /resume   manage Sessions in this Workspace
/rename <title>  /archive  organize the current Session
/status  /workspace        inspect the current state
/config doctor             check whether settings storage is usable
```

Model precedence is:

```text
Session override > Workspace default > User default
```

Saved settings are immediately visible. A currently attached Agent runtime is
pinned to the immutable configuration it started with; a new or recreated
runtime uses a newly resolved configuration. BONE does not claim runtime hot
switching where it has not implemented one.

Only a typed single-line slash command is local. Pasted or multiline text is
always model-visible; use `//text` to send a slash-prefixed message. Unknown
commands stay local and show suggestions.

For one request, pass its text:

```sh
cargo run -p bone-app --bin bone -- --model gpt-5.6 "Read Cargo.toml and list the workspace crates"
```

In interactive mode, `--model` (or `BONE_MODEL`) saves an override on the
Session opened for that launch. In one-shot mode it is an ephemeral input for
that invocation. Neither form creates a hidden fourth settings scope.
`--events session.jsonl` is an explicit new-file export of runtime
observations; it is not BONE's Session database.

## Local data and privacy

All BONE-owned settings, Workspace records, Sessions, drafts, status summaries,
and event histories live in one bundled-SQLite database:

```text
$XDG_DATA_HOME/bone/store-v1/bone.sqlite3
# fallback: ~/.local/share/bone/store-v1/bone.sqlite3
```

The only exception is Rig's ChatGPT OAuth cache, because Rig owns its schema
and token-refresh lifecycle:

```text
$XDG_CONFIG_HOME/bone/store-v1/providers/chatgpt-subscription/auth.json
# fallback: ~/.config/bone/store-v1/providers/chatgpt-subscription/auth.json
```

`bone-store` validates private roots and files, uses SQLite WAL with fail-fast
write contention, and keeps Session writer ownership and provider auth
ownership as separate OS leases. OAuth payloads never enter SQLite, a journal,
debug output, or model-visible tool output.

Older BONE JSON/JSONL/config data is intentionally neither read nor migrated.
It is left untouched; this release starts from the separate `store-v1` root.
See [configuration and storage](docs/configuration.md) for the complete
contract.

## Develop

Start with [the Agent API](docs/agent.md), [configuration and storage]
(docs/configuration.md), [the model API](docs/model-api.md), and the
[TUI architecture](docs/tui-architecture.md).

```sh
cargo fmt --all -- --check
cargo test --workspace --all-features --offline
cargo clippy --workspace --all-targets --all-features --offline -- -D warnings
cargo run -p bone-app --bin bone --offline -- --help
```

The active implementation is in [`crates/`](crates/). [`legacy/`](legacy/)
contains historical material and is deliberately not part of the current
workspace build. [`third_party/`](third_party/) contains the pinned Rig patch.

## Product design

The current product contract is in the [Workspace PRD]
(docs/product/tui-workspace-prd.md), [interaction design]
(docs/product/tui-interaction-design.md), and [runtime architecture]
(docs/product/tui-runtime-architecture.md). A static visual review is available
in the [TUI design](docs/product/bone-tui-design.html).
