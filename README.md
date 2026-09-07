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
                   └─► bone-store
```

- `bone-app` is the composition root: the `bone` binary, Workspace/Session
  domain, settings policy, and the terminal UI.
- `bone-store` is a small generic SQLite backend: keyed documents, journals,
  short transactions, and file leases. It has no Workspace, settings, or
  provider knowledge.
- `bone-app` defines the durable keys and typed records, then injects storage
  into its settings and Workspace/Session services. No other product crate
  depends on `bone-store`.
- `bone-agent` receives two already-connected role models plus immutable Agent
  limits/deadlines. It never opens user storage, selects a provider, or reads
  settings while a runtime is working.
- `bone-llm` owns protocol adapters and non-secret wire configuration. Its
  ChatGPT subscription adapter accepts a narrow application-owned OAuth-cache
  capability rather than discovering a credential directory or storage.
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

The built-in `chatgpt` profile uses a ChatGPT subscription. Add an API-key
profile before selecting a model from it:

```text
/provider                  list saved connection profiles
/provider add openai responses
/provider add gateway chat https://gateway.example/v1
/provider add anthropic anthropic

/model gpt-5.6                     use the built-in ChatGPT profile
/model openai gpt-5.6              set this Session's solver profile/model
/model default gateway my-model    set this Workspace's solver default
/model global anthropic claude     set the user-wide solver default
/model coordinator openai gpt-5.6  set the user-wide coordinator
/model openai gpt-5 --timeout 90 --reasoning-effort high --reasoning-summary concise
/model inherit             remove this Session override
/login                     connect or retry ChatGPT subscription login
/logout                    remove the local login cache when no runtime owns it
/new  /sessions  /resume   manage Sessions in this Workspace
/rename <title>  /archive  organize the current Session
/status  /workspace        inspect the current state
/config doctor             check whether settings storage is usable
```

`responses`, `chat`, and `anthropic` select OpenAI Responses, OpenAI Chat
Completions, and Anthropic Messages respectively. An optional compatible base
URL must be HTTPS. Profile IDs are create-only: use a new ID when changing an
endpoint, then explicitly enter a key for that new profile.

`/model` accepts an optional `--timeout <seconds>` for every profile. The
currently implemented OpenAI Responses controls are explicit too:
`--reasoning-effort <none|minimal|low|medium|high|xhigh|max>`,
`--reasoning-summary <auto|concise|detailed>`, `--reasoning-mode pro`, and
`--reasoning-context <auto|all_turns|current_turn>`. They work with the
Responses protocol (including `chatgpt`) and are rejected for other protocols;
there is no generic raw-JSON parameter escape hatch.

Set an API key outside the TUI so it never becomes command text, SQLite data,
or shell history:

```sh
# This also works before the first interactive TUI launch:
bone provider add openai responses
bone credentials set openai
bone credentials clear openai
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
cargo run -p bone-app --bin bone -- --profile openai --model gpt-5.6 "Read Cargo.toml and list the workspace crates"
```

In interactive mode, `--model` (or `BONE_MODEL`) and optional `--profile` (or
`BONE_PROFILE`) save an override on the Session opened for that launch. In
one-shot mode they are ephemeral inputs for that invocation; an explicit
profile/model pins both coordinator and solver to that one selection. Omitting
a profile uses `chatgpt`. Neither form creates a hidden fourth settings scope.
`--events session.jsonl` is an explicit new-file export of runtime
observations; it is not BONE's Session database.

## Local data and privacy

All BONE-owned settings, Workspace records, Sessions, drafts, status summaries,
and event histories live in one bundled-SQLite database:

```text
$XDG_DATA_HOME/bone/store-v1/bone.sqlite3
# fallback: ~/.local/share/bone/store-v1/bone.sqlite3
```

Connection profiles (ID, protocol, and HTTPS base URL) are non-secret typed
SQLite settings. API keys are stored in the operating system credential
manager under a profile-and-endpoint-bound slot and are never written to
SQLite. Rig's ChatGPT OAuth cache is separately stored because Rig owns its
schema and token-refresh lifecycle:

```text
$XDG_CONFIG_HOME/bone/store-v1/providers/chatgpt-subscription/auth.json
# fallback: ~/.config/bone/store-v1/providers/chatgpt-subscription/auth.json
```

`bone-app` chooses the SQLite data root, the OS credential slot, and the
private ChatGPT cache location. `bone-store` owns WAL, fail-fast write
contention, and generic lease files. API keys and OAuth payloads never enter
SQLite, a journal, debug output, or model-visible tool output.

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
