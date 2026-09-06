# Agent core

`bone-agent` owns one shared session record and one synchronous transition:

```rust,ignore
Kernel::step(event) -> Vec<Effect>
```

The kernel performs no model or tool calls, waits, or clock reads. Runtime starts
and supervises every invocation through the same job mechanism, then returns its
result as an event. Ports execute one call; they cannot hide another agent loop.

## Follow one request

1. `post()` records the user's original message and returns a receipt.
2. An idle solver receives the message directly as `ModelTask::Work`.
3. Its `WorkResult` may reply, call a tool, cancel a job, continue reasoning,
   wait, or finish. Tools do not occupy the solver slot.
4. Tool results and soft reminders return directly to the solver. An
   uninterrupted tool loop makes zero coordinator calls.
5. If a user interrupts an outstanding solver decision, `ReviewInput` handles a
   fixed batch concurrently. It can only keep the work, request reconsideration,
   or pause. Technical reasoning and tool selection remain with the solver.

The coordinator is not an approval stage for solver output. Each solver proposal
passes deterministic validity, pending-input, freshness, and execution checks.
The reply and requirement update are checked together with the operation.
An early result waits for unreviewed input; an obsolete result remains material.
Reconsidered input is explicitly delivered to the next solver batch.

See the [crate guide](../crates/bone-agent/README.md) for use cases and the
[design rationale](agent-model-responsibilities.md) for adversarial examples.

## Integration

```rust,ignore
let agent = Runtime::spawn(model, tools, KernelConfig::default(), RuntimeConfig::default())?;
let mut notices = agent.subscribe();
let receipt = agent.post("Investigate the failing test").await?;
// More input can arrive independently of replies and work completion.
agent.stop().await?;
let report = agent.shutdown().await?;
```

`bone-cli` adapts `bone_llm::Model` and native `bone_tools::Tool` implementations.
It supports concurrent terminal input, `/stop`, and `/exit`.
`SystemConfig.coordinator` configures only input review; the task's solver
performs Work. `TaskConfig`, CLI `--model`, and `BONE_MODEL` override only the
solver. The host resolves settings and injects models at session creation.
There is no shared global conversation or lock spanning model calls.

Read-only cancellation is supervised outside the port Future. The local slot
is released only when that invocation ends. This does not prove remote work
stopped. Writes remain conservatively unresolved until their effects are known.
`Finished { cleanup }` can report abandoned read-only calls, while unknown
writes prevent successful completion. Stop revokes previous model authority;
late tool facts still enter the record. Fresh user input is required to resume.

After a write has returned Unknown, the host can report externally verified
evidence with `resolve_write(id, outcome)`. It accepts only a completed unknown
write and a known tool outcome, then uses the same JobFinished event path.
Repeated identical confirmation is idempotent; conflicting replacements are
rejected. This is not a model command or an automatic reconciliation query.

## Observe execution

`observe().await` atomically returns a snapshot, sequence, and bounded step
stream. Every `StepEvent` contains a sequence, elapsed runtime, input event,
new records, and effect summaries. The stream exposes input reviews, held
results, accepted or discarded proposals, cancellations, and actual outcomes.
Start summaries include message IDs, record position, revision, and generation.

A slow observer cannot block the kernel or keep it alive. `Lagged` explicitly
reports missed steps; call `observe()` for a fresh baseline. Full in-memory
records remain available, but missed raw steps are not replayed. Ordinary
progress is coalesced per job. Observers act through explicit handle commands.

```sh
cargo run -p bone-cli -- --events session.jsonl
```

This independent consumer writes a new JSONL file: baseline `snapshot`, live
`step` records, and explicit `gap` markers. `subscribe()` remains available for
conversational notices.

## Run and verify

```sh
cargo run -p bone-agent --example walkthrough
cargo run -p bone-agent --example interleaving
cargo test --workspace --all-features --locked
```

Controlled tests exercise stale proposals, batching, cancellation, timeouts,
unknown writes, and observation. Time tests use a single-thread Tokio runtime
and virtual time.

The opt-in [live test](../crates/bone-cli/tests/live_agent.rs) uses configured
models, BONE's independent login, and native tools in temporary workspaces:

```sh
BONE_CONFIG='/absolute/path/to/config.json' \
  cargo test -p bone-cli --test live_agent --locked -- --ignored --nocapture
```

It checks direct tool use, status during a pending solver call, changed
requirements, and stop/resume. Controlled delivery may hold an actual result to
exercise a race; it does not replace provider outputs. Inspect the trace and
its stated limits as well as assertions. Do not record device authorization
codes in a saved trace.

The [current acceptance](certifications/bone-agent-2026-09-06-solver-loop.md)
records this refactor's offline checks and real-model interleavings.
The [earlier acceptance](certifications/bone-agent-2026-09-06.md) describes the
previous coordinator-led architecture.
