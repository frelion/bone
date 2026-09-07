# Configuration and local storage

BONE has no user-editable configuration file. The terminal UI is the normal
configuration surface; it writes typed settings immediately and reports the
result through the same event flow as every other TUI effect.

The implementation is deliberately not a generic configuration registry or a
generic key/value database. `bone-store` is a concrete local SQLite service
with a small, typed port used by the product domain.

## What is stored where

All BONE-owned durable data has one source of truth:

```text
$XDG_DATA_HOME/bone/store-v1/bone.sqlite3
# or ~/.local/share/bone/store-v1/bone.sqlite3 when XDG_DATA_HOME is unset
```

The database contains only four internal schema concepts:

| Storage concept | Product use |
| --- | --- |
| `schema_meta` | Store schema version. |
| typed documents | Global settings, Workspace registry/settings, Session records, drafts, and summaries. |
| journals | Strictly ordered Session event facts. |
| OS sidecar leases | Session writer ownership; these are not database tables. |

Document keys are constructed only by domain capabilities. Current product
locations include `settings/global`, the Workspace registry, Workspace
settings, and a Session record/journal under that Workspace. The TUI, model
input, and ordinary crates never construct storage paths or run SQL.

Rig's ChatGPT OAuth cache is the one intentional exception because Rig owns
its JSON schema and refresh-token lifecycle:

```text
$XDG_CONFIG_HOME/bone/store-v1/providers/chatgpt-subscription/auth.json
# or ~/.config/bone/store-v1/providers/chatgpt-subscription/auth.json
```

`bone-store` owns the safe directory/path checks and a fail-fast `auth.lock`.
It never reads, serializes, prints, or stores OAuth payloads in SQLite.

There is no `BONE_CONFIG`, `BONE_STATE_DIR`, `credential_root`, JSON settings
file, JSONL Session transcript, legacy import, or automatic migration. Older
local BONE data is not touched; `store-v1` is a separate clean root.

## Settings users see

`bone-app` owns fixed typed settings rather than independently registered
sections:

```text
GlobalSettings
  agent.coordinator                 optional model selection
  agent.default_solver              optional user-wide default
  agent.soft_deadline_seconds
  agent.shutdown_grace_seconds
  tool_limits
  tui.show_progress

WorkspaceSettings
  default_solver

SessionRecord.metadata
  solver_model_override
```

The solver resolves in the following order:

```text
Session override > Workspace default > User default
```

There is deliberately no guessed model. A new store opens normally with a
Workspace, Session, and editable draft, but reports `NeedsModel` until the user
chooses one.

Use the TUI commands below:

| Command | Durable target | Runtime boundary |
| --- | --- | --- |
| `/model <id>` | current Session override | next newly created/recreated runtime |
| `/model default <id>` | current Workspace default | next newly created/recreated runtime |
| `/model global <id>` | user's global default | next newly created/recreated runtime |
| `/model inherit` | removes current Session override | restores Workspace/User inheritance |
| `/config doctor` | read-only storage health check | no runtime change |

Saving a model setting is live: other Sessions in the same process immediately
recompute their readiness. A runtime that is already attached remains pinned.
BONE resolves a fresh immutable runtime config before accepting a later user
turn; it does not reread settings during Agent execution.

## Runtime configuration and durable turns

Before a user turn is accepted, `SettingsService` overlays global, Workspace,
and Session values into one `ResolvedAgentRuntimeConfig`:

```text
coordinator model
solver model
validated tool limits
soft deadline and shutdown grace
SHA-256 runtime fingerprint
```

The journal records the exact solver and `runtime_fingerprint` from that
resolved object. `UserTurnAccepted` and the Session summary update are written
inside the same SQLite `BEGIN IMMEDIATE` transaction. Only after it commits may
the TUI clear the composer or schedule `AgentHandle::post`.

If storage rejects the transaction—Busy, a revision conflict, a permission
problem, or corruption—the composer remains unchanged and the Agent receives
nothing. SQLite write contention is fail-fast; it is surfaced as `Busy` rather
than blocking the TUI indefinitely.

## Store API and composition

The app opens one `BoneStore` at startup and injects scoped capabilities:

```rust,ignore
let store = BoneStore::open_default()?;
let settings = SettingsService::open(store.clone())?;
let workspace = WorkspaceApplication::open_with_store(launch_dir, store.clone())?;

let global = store.settings().global::<GlobalSettings>();
let state = store.workspace_state();
let auth = store.provider_auth();
```

For tests, portable embedding, and future desktop hosts, supply explicit
absolute roots through `StoreRoots` and `BoneStore::open_at`. This is the only
path injection point; it is not a hidden environment-variable setting.

`Document<T>` supplies missing/read/compare-and-swap replace/remove through a
`Revision`. `Journal<E>` supplies ordered append/read. Coupled Session summary
and event changes use the restricted workspace transaction API, which exposes
typed document/journal operations but not raw SQL or arbitrary keys.

Session writer leases and provider-auth leases are separate, long-lived OS
locks. They identify which process owns a runtime resource; SQLite itself
serializes ordinary document writes.

## Reliability and privacy policy

WAL is a database-wide persisted mode established when the store opens. Every
connection uses foreign keys and a zero busy timeout; every writer configures
`synchronous = FULL` before its short `BEGIN IMMEDIATE` transaction. Read-only
connections deliberately do not apply that write-durability pragma, so a
concurrent TUI read does not spuriously contend with an active writer. Database
files, WAL/SHM sidecars, lock sidecars, and private directories are validated
on Unix for ownership, permissions, symlinks, and hard links. BONE does not
reset, delete, or overwrite a corrupt/unsafe store. The TUI instead enters a
repair/error state.

`/logout` is the only product entry point for deleting the local ChatGPT cache.
It does not revoke an upstream account. If an Endpoint or Model still holds the
provider-auth lease, logout returns Busy and deletes nothing; stop/exit the
owning BONE process before retrying.

Secrets, device codes, refresh tokens, and OAuth payloads never enter global
settings, SQLite documents, Session journals, debug output, notices, or
model-visible tool output.
