# Agent Tool Contract v1

[English](agent-tool-contract.md) | [简体中文](zh-CN/agent-tool-contract.md)

Bone's model-facing tools use Agent Tool Contract v1 to optimize first-call correctness, recoverable failures, and bounded context use. The contract supplements the JSON Schema sent to a model with machine-readable behavior metadata on `ToolDefinition.contract`.

## Input schema

- Use a closed object schema with `additionalProperties: false` at every object boundary.
- Model mutually exclusive operations as a discriminated union. Do not expose one permissive parameter bag and infer an operation from field presence.
- Require the discriminator and the fields needed by that operation. Omit fields that do not apply.
- Reject empty strings, zero identifiers, empty arrays, `null` placeholders, and unbounded collections in the schema.
- Use domain names such as `issueNumber`, `changeNumber`, and `pipelineId`. Provider-specific field names are translated behind the tool boundary.
- Include small valid examples in the contract. Descriptions reinforce the schema but never replace it.

## Result and error contract

Tools return stable, task-oriented projections rather than provider or library response objects. Each contract declares a default and maximum byte budget and may declare an item budget. Large fields use bounded previews or explicit detail operations; truncation remains valid structured data and reports what was omitted.

Execution failures should use a stable code, a concise message, and `retryable: true` or `retryable: false`. Validation errors include only a bounded UTF-8-safe input preview. The Agent loop rejects an unchanged call after deterministic validation or a non-retryable failure. Corrected arguments and retries after transient failures remain allowed.

Extension tools should throw `AgentToolError` rather than encode an error in a successful result or construct JSON manually:

```ts
throw new AgentToolError("rate_limited", "Try again after the provider cooldown", true, {
  retryAfterSeconds: 30,
});
```

Only bounded recovery metadata is model-visible: `retryAfterSeconds`, `statusCode`, `provider`, `resource`, `operation`, `field`, and `requestId`. Provider response bodies, headers, credentials, and arbitrary nested objects must stay in protected logs; unrecognized detail fields are omitted.

The Agent loop converts this error into the shared structured envelope and applies the tool's retry policy. A successful call resets the retry chain. Fingerprints use prepared and validated arguments, so alternate raw spellings that normalize to the same call cannot bypass deterministic-failure protection.

## Effects and idempotency

Every contract classifies the tool as `read`, `routine_write`, `sensitive_write`, or `destructive`. It also declares whether idempotency is inherent, required, or unavailable, and lists the error codes that may be retried with a bounded attempt count.

Effect metadata does not replace runtime authorization. Repository trust, policy gates, interactive confirmation, credentials, and remote permissions are always enforced at execution time.

## Definition example

```ts
const contract = defineAgentToolContract({
  version: 1,
  useWhen: ["Reading one issue from the current repository"],
  doNotUseWhen: ["The issue number is unknown"],
  effect: "read",
  idempotency: "inherent",
  retry: {
    retryableErrors: ["rate_limited", "remote_failure"],
    maxAttempts: 2,
    rejectUnchangedRetry: true,
  },
  outputBudget: { defaultBytes: 16 * 1024, maximumBytes: 64 * 1024 },
  examples: [{ description: "Get issue 42", input: { operation: "get", id: 42 } }],
});
```

Contract changes must test valid calls, polluted or ambiguous calls, deterministic recovery, transient retry behavior, result projection, and UTF-8 byte limits through the same validation and execution paths used by the Agent.
