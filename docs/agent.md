# Agent API

bone-agent is an in-process event-driven core. The Kernel model assigns semantic jobs, full workers solve them, and the code Kernel is the sole state owner. Model and tool ports execute one call; they do not contain private agent loops.

**This rewrite intentionally breaks the old API. bone-app has not been migrated.** Settings storage, credentials, product history and future restart recovery remain outside this crate.

## Construct a runtime

The host connects models before passing them in. The two roles can use the same model instance or independent providers, with separate invocation contexts.

```rust,ignore
use std::time::Duration;
use bone_agent::{
    AgentHost, AgentModels, ConfiguredModel, Input, InputId,
    KernelConfig, ResolvedAgentRuntimeConfig,
};
use bone_tools::ToolLimits;

let models = AgentModels::new(
    ConfiguredModel::without_options(kernel_model),
    ConfiguredModel::without_options(worker_model),
);
let config = ResolvedAgentRuntimeConfig::new(
    ToolLimits::default(),
    KernelConfig::default(),
    Duration::from_secs(5),
)?;
let agent = AgentHost::new(models).start(workspace, config)?;
let receipt = agent.post(Input::new(InputId(1), "Investigate the failure")).await?;
```

ConfiguredModel validates protocol-specific options. AgentHost binds the supplied immutable configuration and installs the existing read/glob/grep tools. It does not read saved settings or initiate authentication.

For custom adapters, use `Runtime::spawn(model_port, tools, kernel_config, runtime_config)`. Adapters must yield while waiting and release local resources when dropped. Tool effect classification comes from the registered adapter, never the model.

## Input, work and execution identities

- InputId is supplied by the host and unique within a runtime. Same ID and identical content return the original receipt; conflicting content is rejected. IDs do not determine chronological order.
- JobId identifies persistent semantic work. One input can affect many jobs, and later inputs can modify an existing job.
- CallId identifies an actual model or tool invocation. EffectId identifies its logical action. This version does not automatically retry business writes or claim universal exactly-once behavior.

A receipt means the Kernel accepted the input in memory, not that it was persisted or completed. Busy inputs are not accepted; the host retains them. A failed interpretation is observable and can be retried with `retry_input(id)`. A clarification or correction uses `Input::new(new_id, text).replying_to(original_id)` and may enter through the reserved path while ordinary admission is closed.

New input combines with still-unresolved original words in acceptance order. The new fixed batch owns interpretation; a late investigation cannot reopen its superseded batch.

## Role boundaries

A `ModelTask::Kernel` returns exactly one `KernelDecision`: changes, optional session constraints, and Apply/Investigate/Clarify. It has no business tools and produces no substantive user answer.

A `ModelTask::Work` returns exactly one `WorkProposal`: public material, optional reply, optional registered tool call, and a next step. Continue uses the background queue; Wait waits for tools or a timer; AskUser exposes a clarification; WaitForResult and WaitForJob express different dependencies. Coordinate requests semantic work-tree changes without acquiring new user authority.

Kernel-model routing is not repeated for ordinary tool results, progress, timers or worker answers. Role contexts preserve raw assigned user text; workers receive their own material and explicit references, not every unrelated job's history.

Session constraints are model-readable instructions. They are not a general machine-enforced authorization language. Only the host and effect-aware adapters can enforce concrete access, data-egress and resource policies.

## Observe, stop and reconcile

`observe().await` atomically returns the snapshot, sequence and a bounded stream of StepEvent. Each step contains its Event, new records and Effect summaries. A slow observer cannot block execution or keep a runtime alive; after Lagged, obtain a new baseline.

Replies carry JobId, input attribution and an as-of cursor. InputHandled means interpretation was committed; InputFinished means all required deliveries reached terminal outcomes. JobFinished is local to that job, not the end of the runtime. Waiting or routing failure has its own notice.

`stop()` revokes previously accepted work and pending routing without waiting for inference. It does not promise that externally authorized work was undone, and it does not revoke input still owned by a caller outside the runtime. Later accepted input may create new work but cannot revive cancelled jobs.

Read-only calls can be abandoned locally. Writes retain None/Applied/Unknown independently of cancellation or Job state. After an Unknown result, only the host may call `resolve_write(call_id, verified_outcome)`. Identical confirmation is idempotent; conflicting confirmation is rejected. Related pending work reconsiders the newly established fact.

`shutdown()` waits for local cleanup within its grace period and returns unresolved_calls, including unknown writes. It is not persistence or remote rollback.

## Verify without a product app

```sh
cargo run -p bone-agent --example walkthrough --locked
cargo run -p bone-agent --example interleaving --locked
cargo test -p bone-agent --all-targets --all-features --locked
```

The examples and controlled-provider tests use the actual Kernel, Runtime and model adapter without a live service or credentials. See the [crate guide](../crates/bone-agent/README.md), [design](agent-realtime-os-design.md) and [scenario map](agent-realtime-os-validation.md). Historical certifications describe their dated implementations, not this rewrite.
