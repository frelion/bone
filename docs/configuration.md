# Configuration and local storage

BONE has no user-editable configuration file. The terminal UI is the normal
configuration surface; it writes typed settings immediately and reports the
result through the same event flow as every other TUI effect.

The implementation is deliberately not a generic configuration registry.
`bone-store` is a small generic SQLite backend; `bone-app` owns the durable
keys, typed records, validation, and configuration policy that use it.

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
locations include `settings/global`, `settings/llm-profiles`, the Workspace
registry, Workspace settings, and a Session record/journal under that
Workspace. The TUI, model input, and ordinary crates never construct storage
paths or run SQL.

Rig's ChatGPT OAuth cache is the one intentional exception because Rig owns
its JSON schema and refresh-token lifecycle:

```text
$XDG_CONFIG_HOME/bone/store-v1/providers/chatgpt-subscription/auth.json
# or ~/.config/bone/store-v1/providers/chatgpt-subscription/auth.json
```

`bone-app` owns the safe credential directory/path checks and a fail-fast
`auth.lock`. It never reads, serializes, prints, or stores OAuth payloads in
SQLite.

API-key profiles use the operating system credential manager instead of a
file. On macOS this is Keychain Services; BONE binds the credential account to
the stable profile ID and a SHA-256 identity of its immutable endpoint config.
The database holds only a profile's ID, label, protocol, and optional HTTPS
compatible base URL—never an API key. This prevents a same-named profile in a
different store from redirecting a saved key to another endpoint. Use
`bone credentials set <profile>` for masked terminal entry or
`bone credentials clear <profile>` to remove it. Keys are never accepted as a
slash-command argument, environment setting, or normal configuration field.

For a headless first-time setup, `bone provider add <id>
<responses|chat|anthropic> [https-url]` creates the same non-secret profile as
`/provider add`; `bone provider list` prints the saved catalog. The key prompt
remains terminal-only. Profile IDs cannot be `default`, `global`,
`coordinator`, or `inherit`, because those words are `/model` scopes.

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

LlmProfiles
  profiles[] { id, label, endpoint }  non-secret provider catalog
```

The solver resolves in the following order:

```text
Session override > Workspace default > User default
```

There is deliberately no guessed model. A new store opens normally with a
Workspace, Session, and editable draft, but reports `NeedsModel` until the user
chooses one.

Each model selection contains a profile ID, model ID, optional
protocol-specific model options, and optional timeout. Existing selections
without a profile migrate naturally to the built-in `chatgpt` profile; legacy
Responses `effort` becomes typed OpenAI Responses options.

Use the TUI commands below:

| Command | Durable target | Runtime boundary |
| --- | --- | --- |
| `/provider` | read profile catalog | no runtime change |
| `/provider add <id> <responses\|chat\|anthropic> [https-url]` | create an immutable API-key profile | no runtime change |
| `/model [profile] <id> [controls]` | current Session solver override | next newly created/recreated runtime |
| `/model default [profile] <id> [controls]` | current Workspace solver default | next newly created/recreated runtime |
| `/model global [profile] <id> [controls]` | user's global solver default | next newly created/recreated runtime |
| `/model coordinator [profile] <id> [controls]` | user-wide coordinator | next newly created/recreated runtime |
| `/model inherit` | removes current Session override | restores Workspace/User inheritance |
| `/config doctor` | read-only storage health check | no runtime change |

The built-in `chatgpt` profile is the ChatGPT subscription connection and is
logged into with `/login`. `responses`, `chat`, and `anthropic` name the three
wire protocols currently exposed by `bone-llm`. App profiles require HTTPS for
compatible endpoints, so an existing API key cannot be sent over plaintext
HTTP. A profile ID is create-only: changing an endpoint requires a new ID and
a deliberate new API-key entry, preventing an existing credential from being
redirected to another host.

When a selected coordinator/solver pair includes API-key profiles, `/login`
checks that all of their keys are available before it starts ChatGPT OAuth or
retries a saved turn. A missing key is reported with its exact `bone
credentials set <profile>` repair command instead of being hidden behind a
second connection failure.

`[controls]` is a small typed surface: `--timeout <seconds>` applies to every
profile; OpenAI Responses profiles additionally accept
`--reasoning-effort <none|minimal|low|medium|high|xhigh|max>`,
`--reasoning-summary <auto|concise|detailed>`, `--reasoning-mode pro`, and
`--reasoning-context <auto|all_turns|current_turn>`. BONE persists these as
`bone_llm::ModelOptions` and rejects them for Chat Completions or Anthropic
profiles before saving. It deliberately has no provider-neutral raw JSON or
arbitrary key/value configuration field.

Saving a model setting is live: other Sessions in the same process immediately
recompute their readiness. A runtime that is already attached remains pinned.
BONE resolves a fresh immutable runtime config before accepting a later user
turn; it does not reread settings during Agent execution.

## Runtime configuration and durable turns

Before a user turn is accepted, `SettingsService` overlays global, Workspace,
and Session values into one non-secret `ResolvedRuntime` plan:

```text
coordinator profile/model/options
solver profile/model/options
validated Agent tool limits and deadlines
SHA-256 runtime-plan fingerprint
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

The app opens one `BoneStore` at startup and injects it into its own domain
services. It separately constructs its credential manager:

```rust,ignore
let store = open_default_store()?;
let settings = SettingsService::open(store.clone())?;
let workspace = WorkspaceApplication::open_with_store(launch_dir, store.clone())?;
let connector = ProviderConnector::new();
```

`bone-app::open_default_store` owns the XDG/default-path policy. For tests,
portable embedding, and future desktop hosts, the App composition layer passes
an explicit absolute root to `StoreRoots` and `BoneStore::open_at`; the generic
store does not inspect environment variables or choose a BONE path.

`Document<T>` supplies missing/read/compare-and-swap replace through a
`Revision`; `Journal<E>` supplies ordered append/read. The App's durable module
maps its Workspace, Session, and settings identities to generic document and
journal keys. Coupled Session summary and event changes use one short generic
store transaction.

Session writer leases and the ChatGPT auth-cache lease are separate,
long-lived OS locks managed by the App. They identify which process owns a
runtime resource; SQLite itself serializes ordinary document writes.

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
ChatGPT cache lease, logout returns Busy and deletes nothing; stop/exit the
owning BONE process before retrying. If this TUI still has a ChatGPT login or
runtime start in progress, it asks the user to wait rather than cancelling that
work or unrelated API-key provider starts.

API keys, device codes, refresh tokens, and OAuth payloads never enter global
settings, SQLite documents, Session journals, debug output, notices, or
model-visible tool output.
