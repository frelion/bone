# GitLab and GitHub integration

[English](forge.md) | [简体中文](zh-CN/forge.md)

Bone includes Forge tools for governed GitLab and GitHub development workflows. GitLab is the primary integration and supports self-managed instances. GitHub provides the core issue, pull request, Actions, milestone, and release workflow.

## Authentication

Open `/settings` and select **GitLab & GitHub** to configure the repository detected from the current Git remote. The page manages the platform, HTTPS API base URL, private-network permission, credential key, and either a direct token or a `$ENV_VAR` reference. Tokens are masked while entered and are stored separately from normal settings in a `0600` credential file.

For public instances, set a fine-grained token in the environment:

```bash
export GITLAB_TOKEN=...
export GITHUB_TOKEN=...
```

Tokens may instead be stored in `~/.bone/agent/forge-auth.json`. Bone refuses to read this file on Unix when group or other permissions are present; set its mode to `0600`.

```json
{
  "gitlab:gitlab.company.com": { "type": "token", "token": "$GITLAB_COMPANY_TOKEN" },
  "github:github.com": { "type": "token", "token": "$GITHUB_TOKEN" }
}
```

Token values are redacted from remote errors and are not included in tool results. Use the minimum scopes required by the intended operations.

## Self-managed GitLab

Every non-public instance must be explicitly allowlisted in `~/.bone/agent/forge.json`:

```json
{
  "version": 1,
  "instances": [
    {
      "provider": "gitlab",
      "host": "gitlab.company.com",
      "apiBaseUrl": "https://gitlab.company.com",
      "credential": "gitlab:gitlab.company.com",
      "allowPrivateNetwork": true
    }
  ]
}
```

Bone accepts HTTPS API endpoints only, does not follow API redirects, and rejects requests whose destination host is not the configured API host. `github.com` to `api.github.com` is the only built-in cross-host mapping.

## Repository policy

A trusted repository can enforce Bone operations with `.bone/forge.yaml`:

```yaml
version: 1
provider: gitlab
workflow:
  issueRequired: true
  branchPattern: "^(feature|fix|chore)/[0-9]+-[a-z0-9-]+$"
  requireCleanWorktreeForReview: true
  requiredLabels: ["workflow::ready"]
  requiredApprovals: 2
  blockUnresolvedDiscussions: true
  requireSuccessfulPipeline: true
  allowMergeMethods: [squash]
  protectedTargets: [main]
  release:
    requireMilestoneClosed: true
    requireTagFromProtectedBranch: true
approvals:
  routineWrites: auto
  sensitiveWrites: confirm
  destructiveWrites: confirm
  nonInteractiveWrites: deny
```

Unknown fields and invalid values are rejected. Project policy is never loaded before project trust is granted. Merge gates are evaluated from fresh remote merge request or pull request state; approval and pipeline facts supplied by the model are not trusted.

Read operations run automatically. Sensitive and destructive mutations require an interactive confirmation by default. They fail closed in print and JSON modes. Completed mutations are journaled by `requestId`; ambiguous network results are not blindly retried.

## Tools

Plan Mode can use `forge_context`, `forge_query`, `forge_audit`, and `forge_watch`. Execution mode additionally provides `forge_issue`, `forge_milestone`, `forge_change`, `forge_wiki`, `forge_pipeline`, `forge_release`, and `forge_transition`.

Forge tools follow [Agent Tool Contract v1](agent-tool-contract.md). `forge_context` takes exactly `{}` and resolves the repository and provider from the current Git working tree. `forge_query` requires one explicit operation:

```json
{"operation":"list","resource":"issue","state":"open","limit":10}
{"operation":"get","resource":"issue","id":42}
{"operation":"get_many","resource":"issue","ids":[42,43]}
```

Do not send unused fields or empty placeholders. Mutation tools likewise expose action-specific closed schemas and domain identifiers. For example, an issue comment uses `issueNumber`, not a generic `id`:

```json
{"action":"comment","requestId":"issue-42-comment-1","issueNumber":42,"input":{"body":"Confirmed by the regression test."}}
```

`requestId` identifies one mutation intent and must be reused only when retrying that exact intent. GitHub and GitLab field differences are translated internally.

`forge_query` returns compact summaries rather than raw provider responses. Lists default to 10 items, accept at most 50, and include at most 384 bytes of body preview per item. Use `search` for provider-side repository text search and `cursor` for another page. Single-detail bodies are limited to 16 KiB; batch-detail bodies are limited to 8 KiB each. Use `parentId` when listing jobs for a pipeline or workflow run. Every Forge tool result has an independent 64 KiB output ceiling with explicit truncation metadata. Invalid argument previews are also bounded, so a failed call cannot echo an arbitrarily large payload into the Agent context.

`forge_context` returns a compact provider identity and negotiates resource capabilities separately instead of assuming that an instance exposes every endpoint. Capability results distinguish supported, unsupported, forbidden, disabled, and unknown behavior. `supported` means the token can access the read endpoint; it does not guarantee permission to create, update, merge, rerun, or release. Write permissions are enforced by the actual mutation and policy gates. Missing advanced capabilities do not disable stable core operations.

GitHub does not publish a supported Wiki REST API, so GitHub Wiki mutations return `unsupported_capability`. GitHub REST also does not expose review-thread resolution; a policy requiring resolved discussions therefore fails closed on GitHub.

## Offline protocol evaluation

Forge includes a deterministic, no-network scripted evaluation harness. It runs the real Agent loop, real Forge schemas and wrappers, and a strict fake `ForgeService`; it does not use tokens or contact GitHub/GitLab. The cases cover a successful query, schema correction, compact mutation receipts, non-retryable correction, bounded retry, large-result budgets, and sequential writes.

Run it from the repository root:

```bash
bun run eval:forge
```

The command writes `.artifacts/forge-eval/report.json` and `.artifacts/forge-eval/report.html`. The JSON is the canonical machine result; the HTML is a human-readable view of each case's tool trace, service trace, context observations, and hard assertions. CI runs the same offline command and uploads both files as the `forge-protocol-eval` artifact. A passing report means the Forge protocol invariants hold. It does not measure general model intelligence, production latency, or GitHub/GitLab version compatibility. Those require separate live-model replay and provider canary evaluations.

## Compatibility

### Opt-in live model evaluation

Run `bun run eval:forge:live -- --model provider/model --runs 3` to evaluate the current Forge contract with a real configured model. The runner uses Bone's `ModelRuntime`, so `/settings`, `models.json`, `auth.json`, OAuth, and custom provider configuration are reused. Without `--model`, it selects the configured default model or the first authenticated model. Forge calls still go to a strict Fake Forge Service: no GitHub/GitLab endpoint is contacted and no mutation is performed. Reports are written separately under `.artifacts/forge-eval/live`.

Use `--max-turns` and `--timeout-ms` to bound cost and runtime. Live reports use task completion, first tool selection, first-call validity, correction success, deterministic repeat rate, mean tool calls, and mean context bytes; they are never merged with scripted `protocolPassRate`. A baseline/candidate comparison requires the same model, prompts, fixtures, and run count against a frozen legacy contract; the live report alone is an absolute measurement, not proof of causality.

GitLab version, edition, license, project settings, feature flags, and token permissions can all affect API behavior. Bone reads `/api/v4/version`, probes resources independently, tolerates additional response fields, validates stable identifiers, and avoids version-based endpoint guessing. A capability that cannot be verified is reported as unknown and is not used for a guarded mutation.
