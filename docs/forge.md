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

`forge_context` negotiates GitLab capabilities separately instead of treating `/api/v4` as a guarantee that every endpoint exists. Capability results distinguish supported, unsupported, forbidden, disabled, and unknown behavior. Missing advanced capabilities do not disable stable core operations.

GitHub does not publish a supported Wiki REST API, so GitHub Wiki mutations return `unsupported_capability`. GitHub REST also does not expose review-thread resolution; a policy requiring resolved discussions therefore fails closed on GitHub.

## Compatibility

GitLab version, edition, license, project settings, feature flags, and token permissions can all affect API behavior. Bone reads `/api/v4/version`, probes resources independently, tolerates additional response fields, validates stable identifiers, and avoids version-based endpoint guessing. A capability that cannot be verified is reported as unknown and is not used for a guarded mutation.
