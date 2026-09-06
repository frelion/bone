# Built-in tools

`bone-tools` contains BONE's provider-independent built-in tool
implementations. The boundary has one owner at each layer:

- `bone-llm` owns the model-facing tool definition, call, output, and replay
  protocol;
- `bone-tools` owns the typed native `Tool` interface and its filesystem,
  process, and configuration implementations;
- `bone-agent` owns asynchronous job execution through its `ToolPort`.

`bone-agent` is the composition root: it configures coordinator and solver
models and concrete tools, then starts the runtime. `bone-tui` uses only
`bone-agent` and `bone-config` from the workspace. In production code, Rig is
confined to `bone-llm`.

The workspace composition overview is kept in the
[README](../README.md#workspace).

The first tool set is deliberately small:

- `read`: paginated, numbered UTF-8 file reads with line, byte, and file-size
  limits.
- `glob`: bounded file discovery using `globset` and a streaming workspace
  walker; respects local ignore files and never enters VCS metadata
  directories.
- `grep`: streaming regex or literal search using ripgrep's Rust libraries;
  supports glob filters, context, binary detection, and bounded results.
- `apply_patch`: Add, Update, Delete, and Move operations using the Codex patch
  grammar. It resolves and validates the entire patch before the first write.
- `bash`: one-shot, non-interactive `bash -c` execution with bounded output,
  deadlines, structured non-zero exits, and Unix process-group cleanup.
- `config`: discover, inspect, validate, replace, and remove registered
  non-secret configuration sections using optimistic revisions. Credentials
  are deliberately unavailable to the model.

Every built-in implements `bone_tools::Tool`. `bone-agent` currently adapts
`read`, `glob`, and `grep` to `ToolPort`, converting validated JSON arguments
and typed outputs at that boundary. An adapter supplies trusted external-effect
metadata.
The local coding tools capture an immutable workspace root and shared hard limits:

```rust,no_run
use bone_tools::ToolEnvironment;

# fn example() -> Result<(), bone_tools::ToolError> {
let tools = ToolEnvironment::new("/workspace/project")?;
let read = tools.read();
let glob = tools.glob();
let grep = tools.grep();
let apply_patch = tools.apply_patch();
let bash = tools.bash();
# let _ = (read, glob, grep, apply_patch, bash);
# Ok(())
# }
```

`ConfigTool` is constructed separately from an `Arc<bone_config::ConfigManager>`
after the host registers the sections it intends to expose. This keeps the
workspace environment independent from application configuration and makes
the registration set an explicit model-facing allowlist.

Native tool calls must run inside an active Tokio runtime. The BONE `Tool`
interface does not make these filesystem and process implementations
executor-agnostic: `bone-agent` schedules calls and sends cooperative cancellation,
while each implementation may enforce a domain-specific deadline. The agent's
soft reminder prompts reconsideration without declaring a tool failed. Bash
uses a sanitized default child environment. A runtime that needs a fully
explicit replacement can construct it separately:

```rust,no_run
use bone_tools::{BashTool, ToolEnvironment};

# fn example() -> Result<(), bone_tools::ToolError> {
let tools = ToolEnvironment::new("/workspace/project")?;
let bash = BashTool::with_process_environment(
    tools,
    [("PATH", "/usr/local/bin:/usr/bin:/bin"), ("LANG", "C.UTF-8")],
)?;
# let _ = bash;
# Ok(())
# }
```

Tool registration, execution outcomes, timeouts, and scheduling belong to
`bone-agent`; provider translation and opaque tool-call correlation remain in
`bone-llm`; concrete behavior belongs to `bone-tools`. Approvals,
authorization, sandboxing, and audit policy remain host responsibilities.

## Safety contract

- Read and search paths may be relative to the workspace or absolute paths
  already inside it. Ordinary path escape and symlink escape outside the
  workspace are rejected.
- Directory searches load `.ignore`, `.gitignore`, and safe
  `.git/info/exclude` files at or below the selected search root. Ignore-file
  size is bounded per file and per call; symbolic links and other unsafe
  ignore sources fail closed for that directory and produce a warning. Parent
  ignore files above the selected root and user/global Git excludes are not
  imported. With `include_hidden = false`, hidden paths are filtered only when
  no ignore rule matches; an explicit ignore whitelist can include one. Every
  encountered filesystem entry counts toward the traversal limit, including
  hidden and ignored entries. Results retained before a limit are sorted, but
  when traversal or result limits truncate a search the selected subset can
  depend on filesystem enumeration order.
- Patch paths are stricter: they must be relative, may not contain `..`, and
  may not pass through an existing symlink. Add and Move never overwrite an
  existing target.
- Patch parsing, path checks, source snapshots, context matching, collision
  checks, concurrent-change validation, replacement files, and rollback copies
  finish before the first target-file mutation. Staging uses temporary files and
  may create missing parent directories; a failed patch can leave those new
  directories behind if they are empty. Commits use same-directory atomic
  replacement where applicable and roll back earlier target-file actions if a
  later action fails. This is still not a transactional filesystem operation:
  an exceptional rollback failure can leave changes behind, and the error
  reports the affected workspace-relative paths while retaining recovery
  copies when possible. Cancelling the calling future detaches the in-flight
  transaction so it can finish rollback; process crashes and runtime teardown
  remain outside this in-memory guarantee. Some Unix fallback filesystems may
  also retain a hidden staging hard link after an otherwise successful Add or
  Move if cleanup of the temporary name fails.
- Bash's workspace check constrains only its initial working directory. Bash
  can still access host paths, processes, and the network unless the runtime
  applies an OS sandbox. It intentionally contains no command-string denylist.
  The child environment is cleared by default, then only common path, locale,
  terminal, temporary-directory, and basic Windows runtime variables are copied
  from the host. In particular, `HOME`, `BASH_ENV`, proxy variables, cloud and
  provider credentials, and SSH-agent variables are not inherited. A runtime
  can supply a complete replacement environment explicitly; it should not put
  secrets there because commands can print them. Environment clearing also
  cannot prevent access to host secrets through files, process inspection, or
  other OS resources, so it does not replace sandboxing. Unix builds clean up
  the command's process group; the non-Unix fallback only guarantees
  direct-child cleanup, so production Windows hosts need a Job Object or
  equivalent runtime wrapper.
- Workspace path checks are not a capability filesystem and cannot close every
  hostile concurrent path-replacement race. A runtime executing untrusted code
  must add a capability filesystem or OS sandbox.

`ToolLimits` owns the `tools.local` configuration section. Omitted fields use
its existing defaults. `bone-agent::start` reads the section from its startup
snapshot and passes it to `ToolEnvironment::with_limits`; later changes affect
new sessions. The JSON fields `default_bash_timeout_seconds` and
`max_bash_timeout_seconds` use positive integer seconds. Persisting subsecond
Rust durations returns an error instead of truncating them.

Model-requested limits can only narrow their corresponding hard limits.
`max_output_bytes` applies to read/search output and to each Bash stream. Patch
summaries are instead bounded indirectly by `max_patch_bytes` and
`max_patch_files`. All text limits apply before JSON encoding; JSON field
overhead and escaping are the agent host's final context-budget responsibility.
