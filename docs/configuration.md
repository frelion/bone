# Configuration

BONE keeps non-secret configuration behind one typed service. Human-facing
clients can call that service directly, while agents use the `config` tool in
`bone-tools`; neither interface parses or writes configuration independently.

## Storage

`ConfigManager::builder()` registers sections and `build` accepts an explicit
absolute JSON path supplied by the host. A normal application host should use
its platform configuration directory, for example
`$XDG_CONFIG_HOME/bone/config.json` on Unix. The file is a top-level map from
registered section names to complete section values:

```json
{
  "tools.local": {
    "max_output_bytes": 51200
  },
  "tools.forge": {
    "default_connection": "github-work"
  }
}
```

Each consumer owns a typed `ConfigSection` with a stable key, description,
JSON Schema, and validation function. `bone-config` owns storage and does not
depend on providers or tools. Unknown sections and invalid section values are
rejected when a configuration is loaded or changed.

A snapshot is immutable and carries an opaque revision derived from the whole
configuration. Replacing or removing a section requires the revision the
caller read. The manager takes a cross-process lock, rereads and validates the
file, compares the revision, and atomically replaces the file. A stale writer
receives a conflict instead of overwriting newer configuration. Lock contention
returns a retryable `Busy` error instead of waiting without a deadline.

New files are private by default, but existing non-secret configuration does
not have to be mode `0600`; this keeps it usable in ordinary human-managed and
shared workspaces. Regular-file and no-final-symlink checks still guard the
atomic persistence boundary, and an existing file's Unix permission bits are
preserved across writes. Strong no-follow guarantees currently apply on Unix;
other platforms require equivalent protection from the host environment.

There is deliberately no implicit environment overlay, project inheritance,
deep merge, or hot mutation of existing provider and tool instances. The host
constructs those instances from a snapshot and decides when to rebuild them.

## Agent model settings

`bone-cli` owns the `SystemConfig` section keyed by `agent.system` and loads
it through `ConfigManager`. It contains `coordinator` and `default_solver`,
each with a model ID, optional reasoning `effort`, and `timeout_seconds`
(positive, default 120). See the [example](../crates/bone-cli/config.example.json).

The coordinator handles only busy-time input review (`ReviewInput`); the solver
owns task reasoning, tools, and delivery (`Work`). The coordinator is a system
setting injected when the host creates a session.
It is not part of task configuration. `TaskConfig` overrides only solver
settings, leaving the system snapshot and file unchanged. In the CLI, solver
model selection is `--model`, then `BONE_MODEL`, then the system default;
model-only overrides retain the default solver's effort and deadline.
This precedence is resolved explicitly by the CLI, outside the storage service.

The CLI reads `$XDG_CONFIG_HOME/bone/config.json`, falling back to
`$HOME/.config/bone/config.json` when that directory variable is unset or empty.
`BONE_CONFIG` can supply another absolute path. Task text and the workspace
directory do not select the system configuration. The configuration manager
is kept by the host and is not exposed as a task tool. Existing sessions keep
their injected models and settings until shutdown.

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
