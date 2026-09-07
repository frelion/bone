# TUI runtime architecture

> **Current implementation contract.** This document describes the runtime
> architecture shipped with the SQLite-backed `bone-store` design. Earlier
> product sketches that mention editable config files, `bone-config`, JSONL
> transcripts, configuration revisions, or a credential-root setting are
> historical only and must not be treated as operational instructions.

The BONE terminal UI is a durable workspace workbench. Opening `bone` in a
directory selects that directory as a Workspace; it does not create a project
metadata directory or infer a Git root. A Workspace can contain many durable
Sessions. The TUI restores their conversation facts and drafts from storage,
then attaches process-local Agent runtimes only when they are needed.

## Ownership at a glance

```text
terminal input / Agent observations / async effect outcomes
                         |
                         v
                    AppEvent
                         |
                         v
                    App reducer
                         |
                  Action / intent
                         |
                         v
       effect layer (storage, connection, Agent runtime)
                         |
                         v
                    AppEvent
```

The reducer owns only presentation state: visible Sessions, selected tab,
composer text, focus, notices, connection state, and projected conversation.
It owns no SQLite connection, lock, `AgentHandle`, endpoint, observer task, or
filesystem path. Effects own those resources and report results back through
`AppEvent`. This is the TUI's unidirectional data flow: neither terminal input
nor background tasks mutate presentation state directly.

| Layer | Owns | Does not own |
| --- | --- | --- |
| `bone-store` | one SQLite database, generic keyed documents/journals/leases | product models, key layout, OAuth payloads |
| `bone-app::durable` | Workspace identity, Session records, journal facts, writer lease policy | JSON/JSONL files, a second session index |
| `SettingsService` | typed settings/profile validation and model overlay | runtime storage reads or provider credentials |
| TUI reducer | deterministic presentation state | persistence, runtime handles, locks |
| TUI effect driver | durable commands, login connection, runtime lifecycle | direct UI mutation |
| `bone-agent` | immutable Agent limits/deadlines and execution of injected role models | user settings documents, profiles, or local storage |
| `bone-llm` / Rig | protocol adapters, endpoint construction, and OAuth refresh behavior | provider-profile persistence or cache deletion policy |

## Startup and composition

The binary opens exactly one `BoneStore` for the application instance. It
passes clones only to its Workspace and settings services; those App-owned
services define the keys and typed records. `ProviderConnector` opens the
App-owned ChatGPT credential manager lazily only when a selected profile needs
it, rather than letting feature crates discover a storage root.

```rust,ignore
let store = open_default_store()?;
let application = WorkspaceApplication::open_with_store(current_dir, store.clone())?;
let settings = SettingsService::open(store.clone())?;
let connector = ProviderConnector::new();
```

`bone-app::open_default_store` uses the XDG data root for BONE data:

```text
$XDG_DATA_HOME/bone/store-v1/bone.sqlite3
# or ~/.local/share/bone/store-v1/bone.sqlite3
```

The first interactive launch initializes the SQLite schema and typed global
settings, discovers/creates the Workspace record, then restores or creates an
editable Session draft. It intentionally does **not** choose a model or log
the user in. A valid workbench with no resolved model projects `NeedsModel` and
offers configuration commands rather than failing before the terminal starts.

Tests, portable hosts, and future desktop hosts inject an explicit absolute
data root through the App composition layer. The generic Store only accepts
that root; there are no BONE-specific path override environment variables.

## Durable Workspace and Session lifecycle

A canonical launch-directory identity maps to one opaque `WorkspaceId`.
The Workspace registry is a typed document in the same database. Session
records are listed by their fixed Workspace key prefix; the sidebar never
maintains a separate JSON index or cache source of truth.

```text
launch directory
  -> canonical Workspace identity
  -> WorkspaceId
  -> Workspace settings + ordered Session records
  -> selected Session writer lease (if available)
  -> durable draft + journal hydration
```

The selected Session gets a fail-fast, process-lifetime writer lease. If
another BONE process owns it, the UI can still hydrate and show it as read-only;
it does not block or steal ownership. Startup tries another active Session and
can create a fresh Session if all candidates are owned elsewhere. SQLite
transactions—not writer leases—serialize ordinary short document writes.

Session draft saves, rename/archive changes, state transitions, and model
overrides use a typed Session document with SQLite revision compare-and-swap.
The storage revision is the only optimistic-concurrency token. The Session
model does not serialize a duplicate revision value.

## Atomic turn acceptance

The durable acceptance boundary is `UserTurnAccepted`. Preparing a post first
resolves and freezes runtime configuration. The effect layer then writes the
following in one short SQLite `BEGIN IMMEDIATE` transaction:

1. validate that this process still owns the Session writer lease;
2. append `UserTurnAccepted` to the Session journal;
3. update the Session summary, pending/execution state, and storage revision;
4. commit all changes together.

The journal event records both the actual solver model and the SHA-256
`runtime_fingerprint` from the same non-secret `ResolvedRuntime` plan that is
used to construct its Agent models/configuration. There is no raw whole-config
hash and no later runtime-time reread that can drift from the durable fact.

Only after a successful commit does the effect report `TurnAccepted`; the
reducer then projects the user message and clears the matching composer. If
SQLite is Busy, a revision conflict occurs, or the store fails validation, the
effect reports `TurnRejected`: the composer remains intact and no Agent post is
queued. A journal append and a Session summary can never be half committed.

```text
composer submit
  -> resolve immutable runtime config
  -> SQLite transaction: journal fact + Session state
  -> reducer receives TurnAccepted
  -> connect/start runtime if necessary
  -> AgentHandle::post(the already accepted text)
```

An accepted turn may temporarily be durable but not delivered—for example,
while provider login or runtime startup fails. It remains explicitly pending
and retryable; it is never accepted or projected a second time merely because
an effect is retried.

## Settings and runtime pinning

The user configures BONE through commands, not a text editor. Settings have
fixed typed ownership:

```text
GlobalSettings.agent.default_solver       user-wide default
GlobalSettings.agent.coordinator          user-wide coordinator
WorkspaceSettings.default_solver          Workspace default
SessionRecord.solver_model_override       Session override
LlmProfiles                               non-secret provider catalog
```

Resolution is deterministic:

```text
Session override > Workspace default > User default
```

`/provider add <id> <protocol> [https-url]` creates a non-secret profile.
`/model [profile] <id> [controls]` saves the current Session solver override;
`/model default` and `/model global` save the corresponding solver defaults;
`/model coordinator [profile] <id>` saves the user-wide coordinator;
`/model inherit` removes the current Session override. The UI refreshes its
readiness immediately after each successful save. These settings govern a new
or recreated runtime; an already attached runtime remains pinned to its
existing immutable configuration. Runtime hot-switching is deliberately
outside this architecture.

For one-shot CLI use, an explicit `--profile` plus `--model` is a complete
ephemeral role selection: it supplies both solver and coordinator, so it never
silently requires a separately saved provider credential. Without an explicit
selection, the normal saved coordinator/default-solver resolution applies.

`[controls]` is intentionally typed: every profile accepts `--timeout`, while
OpenAI Responses profiles accept the supported `--reasoning-*` controls. The
App persists `ModelOptions` and validates its protocol before a runtime is
created; it does not carry an arbitrary provider parameter map.

Before an Agent starts, `SettingsService` overlays the settings into a complete
non-secret `ResolvedRuntime`: coordinator/solver profile selections, validated
tool limits, deadlines, and fingerprint. The App connector turns each selected
profile into an in-memory `ConfiguredModel`, then passes `AgentModels` and the
Agent-only `ResolvedAgentRuntimeConfig` to `AgentHost`. The Agent never
receives a `BoneStore`, profile, or credential.

## Provider connection and logout

Profiles (ID, protocol, optional HTTPS base URL) live as non-secret typed
SQLite settings. API keys live in the operating system credential manager and
are read only while constructing an endpoint. ChatGPT subscription OAuth is a
separate exception because Rig owns its `auth.json` schema and refresh
lifecycle. The cache lives under the private XDG config root:

```text
$XDG_CONFIG_HOME/bone/store-v1/providers/chatgpt-subscription/auth.json
# or ~/.config/bone/store-v1/providers/chatgpt-subscription/auth.json
```

The connection effect acquires a `ChatGptAuthLease` before calling Rig and
serializes OAuth connection/refresh within the process. The lease holds a
fail-fast exclusive `auth.lock`; it is retained by derived model handles and
prevents a second process from concurrently owning the same provider cache.
The lease exposes only a previously validated path. It never parses, logs, or
serializes OAuth data.

`/logout` calls `ChatGptCredentials::clear`. If any endpoint/model still
holds the lease, it reports Busy and changes nothing. A successful logout only
removes the local OAuth cache; it does not claim to revoke an upstream account.
The TUI also leaves an in-progress ChatGPT startup intact and asks the user to
retry logout when it finishes; starts for API-key profiles are never cancelled
by a ChatGPT logout.

Before `/login` starts OAuth or retries a turn, the TUI checks every selected
API-key profile. It reports missing keys with the exact `bone credentials set
<profile>` command first; a mixed profile plan cannot complete ChatGPT login
only to fail immediately on an unchecked API key.

## Failure and observation behavior

WAL is established as a persisted database mode when the store opens. Every
connection uses foreign keys and a zero busy timeout; every connection that
can write configures full synchronous writes before its short transaction.
Read-only connections deliberately avoid that connection-local write pragma so
they retain WAL's concurrent-read behavior beside an active writer. Private
database directories/files, SQLite sidecars, and lease files are checked for
unsafe permissions, ownership, symlinks, and hard links on Unix. BONE never
deletes or resets an unsafe, corrupt, or unsupported store automatically. The
UI presents a storage repair/error condition instead.

The runtime driver fans Agent observations into tagged `RuntimeStep`,
`RuntimeReset`, and `RuntimeClosed` events. It reconstructs a lost broadcast
stream from the Agent snapshot, then lets the reducer project the result. A
background observer therefore cannot make an unsynchronized UI change.

## Invariants worth preserving

- One app instance opens one `BoneStore`; only App-owned persistence services
  receive it, while feature crates receive resolved values or narrow capabilities.
- BONE-owned data has one SQLite source of truth. JSON appears only as an
  internal serialized payload column, not a user-facing persistence format.
- The reducer is the only writer of TUI presentation state.
- A Session writer lease governs long-lived runtime ownership; SQLite governs
  short durable writes.
- A turn's journal attribution and Agent runtime derive from the same frozen
  non-secret runtime plan.
- API keys, OAuth payloads, device codes, and refresh tokens never enter
  documents, journals, notices, debug output, or model-visible tool output.
- Persistence failures preserve user input and never silently dispatch work.
