# Agent API

`bone-agent` owns the synchronous Kernel state machine and the asynchronous
runtime that executes its model and tool effects. It deliberately owns neither
user settings, SQLite storage, Workspace records, nor provider authentication.
Those concerns are composed by `bone-app`.

```rust,ignore
Kernel::step(event) -> Vec<Effect>
```

The Kernel performs no model/tool call, wait, clock read, configuration read,
or filesystem persistence. Runtime supervises every invocation through the
same job mechanism and returns results as events. Ports execute one call; they
cannot hide a second agent loop.

## Start a pinned runtime

The product resolves persistent settings and authenticates role models before
constructing an Agent runtime:

```rust,ignore
use bone_agent::{AgentHost, AgentModels, ConfiguredModel, ResolvedAgentRuntimeConfig};

let coordinator = ConfiguredModel::new(connect_coordinator_model().await?, None)?;
let solver = ConfiguredModel::new(connect_solver_model().await?, None)?;
let config: ResolvedAgentRuntimeConfig = resolve_settings_for_this_session()?;
let host = AgentHost::new(AgentModels::new(coordinator, solver));
let agent = host.start(workspace, config)?;

let receipt = agent.post("Investigate the failing test").await?;
agent.stop().await?;
let report = agent.shutdown().await?;
```

`AgentHost::start(workspace, config)` is synchronous because it only builds
local tools, Kernel, and Runtime. It does not read disk settings or initiate
login. The supplied `ResolvedAgentRuntimeConfig` is complete and immutable,
but owns only Agent execution policy:

```text
validated ToolLimits
soft deadline, review timeout, work timeout, shutdown grace
```

`ConfiguredModel` fixes a selected `bone_llm::Model` and its validated
protocol-specific `ModelOptions`; it rejects options for another protocol.
Later settings changes cannot mutate an attached runtime. Product code resolves
a new non-secret runtime plan and starts/recreates a runtime at an explicit
lifecycle boundary. `bone-app` records its own plan fingerprint and solver in
the durable Session journal before delivering a user turn.

Endpoints are connected by `bone-app` from its saved profile plus a Keychain
API key or `ChatGptCredentials` lease. The resulting models retain only their
in-memory client/lease. `bone-agent` never sees a profile, OAuth path,
credential root, API key, or token payload.

For controlled ports and embedded custom execution,
`Runtime::spawn(model, tools, kernel_config, runtime_config)` remains
available. No model invocation shares mutable conversation state or a lock
spanning a request.

## Follow one request

1. `post()` records the user's original message and returns a receipt.
2. An idle solver receives the message directly as `ModelTask::Work`.
3. Its `WorkResult` may reply, call a tool, cancel a job, continue reasoning,
   wait, or finish. Tools do not occupy the solver slot.
4. Tool results and soft reminders return directly to the solver. An
   uninterrupted tool loop makes zero coordinator calls.
5. If a user interrupts an outstanding solver decision, `ReviewInput` handles
   a fixed batch concurrently. It can keep work, request reconsideration, or
   pause; technical reasoning and tool selection remain with the solver.

The coordinator is not an approval stage for solver output. Each solver
proposal passes deterministic validity, pending-input, freshness, and execution
checks. The reply and requirement update are checked together with the
operation. An early result waits for unreviewed input; an obsolete result
remains material. Reconsidered input is explicitly delivered to the next solver
batch.

Read-only cancellation is supervised outside the port Future. Releasing a
local slot does not prove remote work stopped. Writes remain conservatively
unresolved until their effects are known. `Finished { cleanup }` can report
abandoned read-only calls, while unknown writes prevent successful completion.
Stop revokes previous model authority; late tool facts still enter the record.
Fresh user input is required to resume.

After a write has returned Unknown, the host can report externally verified
evidence with `resolve_write(id, outcome)`. It accepts only a completed unknown
write and a known tool outcome, then uses the same `JobFinished` event path.
Repeated identical confirmation is idempotent; conflicting replacements are
rejected. This is not a model command or automatic reconciliation query.

## Observe execution

`observe().await` atomically returns a snapshot, sequence, and bounded step
stream. Every `StepEvent` contains a sequence, elapsed runtime, input event,
new records, and effect summaries. The stream exposes input reviews, held
results, accepted/discarded proposals, cancellations, and actual outcomes.

A slow observer cannot block the Kernel or keep it alive. `Lagged` explicitly
reports missed steps; call `observe()` for a fresh baseline. Full in-memory
records remain available, but missed raw steps are not replayed. Ordinary
progress is coalesced per job. Observers act through explicit handle commands.

The `bone` CLI can independently export an observation stream to a newly
created JSONL file:

```sh
cargo run -p bone-app --bin bone -- --model gpt-5.6 --events session.jsonl "Inspect the workspace"
```

That export is a requested artifact (snapshot, live steps, explicit gaps), not
the durable Session history. Durable product data belongs exclusively to
`bone-store`; see [configuration and storage](configuration.md).

## Run and verify

```sh
cargo run -p bone-agent --example walkthrough
cargo run -p bone-agent --example interleaving
cargo test --workspace --all-features --offline
```

Controlled tests exercise stale proposals, batching, cancellation, timeouts,
unknown writes, and observation. Time tests use a single-thread Tokio runtime
and virtual time. The [crate guide](../crates/bone-agent/README.md) gives
focused use cases; [agent model responsibilities](agent-model-responsibilities.md)
contains the design rationale and adversarial examples.

The ignored ChatGPT subscription live certification is owned by `bone-app` and
uses its local ChatGPT credential cache described in [provider testing]
(provider-testing.md). Never record device authorization codes, OAuth payloads,
or provider tokens in a trace.
