# BONE

BONE is a Rust workspace for building coding agents. The current product layer
is headless: `bone-app` assembles models, tools, persistence, and `bone-agent`
behind a frontend-neutral Rust API.

The previous terminal UI and `bone` CLI have been removed. A new TUI can be
built as a separate frontend crate over the same `App` and `Session` API used
by future desktop, web, or automation clients.

## Architecture

```text
future frontends
      │
      ▼
  bone-app ───────► bone-agent
      ├───────────► bone-llm
      ├───────────► bone-tools
      └───────────► private storage/ (SQLite, journals, leases)
```

- `bone-app` owns configuration, profiles and credentials, durable workspaces
  and sessions, Agent assembly, write-effect tracking, and runtime lifecycle.
- `bone-agent` owns input routing, Job state, scoped context, model/tool calls,
  control, and structured records.
- `bone-llm` owns provider-independent model requests and wire adapters.
- `bone-tools` owns workspace-local read, search, patch, and process tools.

The former `bone-store` crate now lives in `bone-app`'s private `storage/`
module. Frontends see product objects and errors rather than documents,
journal keys, transactions, or SQLite types.

## Application API

`App` is the composition root. It opens workspaces, creates and retrieves
sessions, manages configuration and credentials, and shuts down live
runtimes. `Session` is the execution handle a frontend retains.

```rust,no_run
use bone_app::{App, AppOptions, SessionSeq, SubmitInput};

# async fn example() -> bone_app::Result<()> {
let app = App::open(AppOptions::new("/absolute/path/to/app-data")).await?;
let workspace = app.open_workspace("/absolute/path/to/workspace").await?;
let session = app.create_session(workspace.id, "New session").await?;

let mut view = session.observe();
let receipt = session.submit(SubmitInput::new("Inspect this workspace")).await?;
let history = session.history(SessionSeq(0), 100).await?;

# let _ = (&mut view, receipt, history);
app.shutdown().await?;
# Ok(())
# }
```

Submitting succeeds once the input is durable. Execution may remain queued
until a frontend saves a valid model selection; that configuration update
reapplies automatically. After repairing an external prerequisite such as
login, call `Session::reload_config` to reapply the already saved desired
configuration. Asynchronous startup failures are published as typed
`AppProblem` values, so a frontend can react to `LoginRequired(ProfileId)`
without parsing display text.

Current state arrives through `Session::observe`, a Tokio `watch` receiver.
Durable events come from `Session::history(after, limit)`. Clients keep the
returned cursor and can recover after disconnecting without relying on an
in-memory event stream. Runtime-local Job and Call IDs are wrapped with a
`RuntimeId`, so stale control requests cannot affect a replacement runtime.

Configuration is typed and resolves in this order:

```text
Session override > Workspace override > User setting
```

`App::update_config(scope, change)` saves one explicit change and returns only
after every affected open Session has applied it. A running runtime keeps its
`RuntimeId`, job graph, and in-flight tool calls while model work restarts on
the new configuration. If the new configuration cannot be assembled, the
Session stays suspended until a valid update arrives; it never falls back to
the old configuration for new work.

## Persistence

The host chooses an absolute data directory through `AppOptions`. `bone-app`
stores its SQLite database at `<data_dir>/bone.sqlite3`; it does not impose an
XDG or project-local path.

SQLite stores workspace and session records, input idempotency, runtime
configuration snapshots, durable Agent records, history, and write attempts.
API keys remain in the operating-system credential manager. A write remains
blocking until its result is durably acknowledged by the matching Agent fact;
after a crash, the host resolves any unacknowledged write explicitly.
`App::unresolved_writes` is the authoritative cross-Session query.

Process restart restores product state and marks work lost with its runtime as
interrupted. It does not restore in-memory Job futures or replay uncertain
external writes.

## Validation

```sh
cargo fmt --all -- --check
cargo test -p bone-app --lib --locked
cargo test --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
```

There is currently no `cargo run -p bone-app` command because `bone-app` has
no binary target.

Start with the [App architecture](docs/bone-app-design.md),
[Agent API](docs/agent.md), [model API](docs/model-api.md), and
[built-in tools](docs/tools.md). Documents under `docs/product/` and the old
TUI architecture describe the removed frontend and are retained as historical
design input for its future standalone replacement.

The active implementation is in [`crates/`](crates/). [`legacy/`](legacy/)
contains historical material and is not part of the workspace build.
[`third_party/`](third_party/) contains the pinned Rig patch.
