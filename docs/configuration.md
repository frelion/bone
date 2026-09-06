# Configuration

`bone-config` is the shared configuration store. Each module defines its own
`ConfigSection`; the store handles registration, validation, snapshots, and
atomic writes without depending on those modules.

## One file, module-owned sections

| Section | Owner | Settings |
| --- | --- | --- |
| `agent.system` | `bone-agent` | Coordinator, default solver, model deadlines, tool reminder, shutdown grace. |
| `llm.system` | `bone-llm` | Optional `credential_root` for the current ChatGPT connection. |
| `tools.local` | `bone-tools` | `ToolLimits`, including output, read, search, and shell limits. |
| `tui.display` | `bone-tui` | `show_progress`, default true. |

See the [complete example](../crates/bone-tui/config.example.json). Only
`agent.system` is required; the other sections and individual tool limits use
defaults when omitted. Model IDs must be selected explicitly.

`bone_config::default_path()` resolves `BONE_CONFIG`, then
`$XDG_CONFIG_HOME/bone/config.json`, then `$HOME/.config/bone/config.json`.
Selected paths must be absolute. `ConfigManager::builder().build(path)` still
accepts an explicit path for embedded applications and tests.

```rust,ignore
let config = bone_agent::config_builder()?
    .register::<bone_tui::TuiConfig>()?
    .build(bone_config::default_path()?)?;
```

Agent's builder registers Agent, LLM, and Tools settings. TUI adds only its
presentation settings. Registration is complete before `build`; each manager
then has a fixed set of known types and schemas.

## Reading and writing

A snapshot contains one complete file revision. Registered sections are
validated through Serde and the module's `validate()` function. JSON Schema is
available for editors; it does not replace those checks.

Unregistered sections remain in the snapshot and are preserved during writes.
Their values have not been validated by this manager. They can be listed with
`unrecognized_sections()`; the terminal reports their names. Typed reads and
mutations require registration, so one component cannot silently edit an
unknown section. Misspelled required section names still produce a missing
configuration error.

```rust,ignore
let snapshot = config.snapshot()?;
let settings = snapshot.get::<bone_tui::TuiConfig>()?.unwrap_or_default();
let change = config.set(&settings, snapshot.revision())?;
```

Writes replace one complete section. The store locks, rereads the whole file,
checks the expected revision, and atomically replaces it. A stale writer gets
`RevisionConflict`; lock contention returns `Busy`. Unknown sections and other
modules' values survive the operation. There is no implicit merge or retry.

## When settings take effect

`bone_agent::start()` reads one new snapshot and resolves the task's solver,
LLM connection, tool limits, and runtime settings from it. Existing sessions
retain their captured settings. A saved change applies to the next session.
The subscription connection currently allows one active session per credential
root; close the old session before reopening it with that root.

Coordinator selection is system-level. `TaskConfig` can override only the
solver model, effort, and deadline. The terminal resolves `--model`, then
`BONE_MODEL`, then the system default; overrides do not write back to the file.

Agent's `soft_deadline_seconds` defaults to 30 and `shutdown_grace_seconds` to
5. Model `timeout_seconds` defaults to 120. Tool durations are persisted as
integer `default_bash_timeout_seconds` and `max_bash_timeout_seconds`; the Rust
API still uses `Duration`. Saving a fractional second value is rejected rather
than rounded.

`llm.system.credential_root` selects the existing OAuth storage directory; it
is not a credential value. Omitting it retains BONE's existing login path.
The model library's protocol constructors remain available; this application
entrypoint currently connects the ChatGPT subscription service.

TUI reads display preferences at frontend startup. The store returns a
`ConfigChange` with the saved revision; it does not send reload events or mutate
running model/tool instances.

## Credentials

Secret values are not configuration values. Configuration contains only a
credential key such as `github.work`; a separate `CredentialStore` resolves
that key to a redacted `SecretLease`. Secret values and leases are not
serializable and never expose their contents through `Debug`.

The credential store uses a separate private JSON file, a fail-fast exclusive
lock, and same-directory atomic replacement. On Unix it also verifies
ownership, file type, link count, and private permissions. A future UI or CLI
may call the credential API directly. Secret values must never pass through an
agent tool argument, result, transcript, or model-visible error. The host must
supply a trusted parent directory; on non-Unix systems it is also responsible
for choosing a directory protected by the platform's ACLs. The store resolves
the supplied parent to a stable absolute path once and revalidates that parent
before every operation; it is not a sandbox against a malicious process
running as the same operating-system user.

## Agent tool

The single `config` tool has five closed actions:

- `list`: list registered section names, descriptions, and whether configured.
- `get`: return one configured non-secret section and the current revision.
- `schema`: return the declared JSON Schema for one section.
- `set`: validate and replace one complete section using an expected revision.
- `remove`: remove one complete section using an expected revision.

Calls use a stable root object, for example
`{"request":{"action":"get","section":"tools.forge"}}`. Keeping the
action union below that root makes the definition portable across strict model
provider schemas. Tool results have a host-selectable byte ceiling (50 KiB by
default); oversized JSON is rejected as a whole rather than truncated.

`set` and `remove` are configuration writes. Approval and authorization happen
in the future runtime before dispatch, just as they do for file writes and
shell execution; they are not hidden inside the storage service. The tool
cannot read or write credential values. The host should register only sections
that may be shown to the agent and must enforce per-call policy before
dispatch. Once a write reaches blocking storage, cancelling the async caller
does not imply rollback; the caller should read again after an uncertain
outcome.
