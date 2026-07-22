# GitLab 与 GitHub 集成

[English](../forge.md) | [简体中文](forge.md)

Bone 内置 Forge 工具，用于执行受研发规范约束的 GitLab 与 GitHub 工作流。GitLab 是首要集成目标并支持私有化部署；GitHub 覆盖 issue、Pull Request、Actions、milestone 和 release 核心流程。

## 认证

打开 `/settings` 并选择 **GitLab & GitHub**，即可为当前 Git 远端自动识别出的仓库配置平台、HTTPS API Base URL、私有网络访问权限、凭据 Key，以及直接 Token 或 `$ENV_VAR` 引用。Token 输入过程会被遮罩，并与普通设置分开保存在权限为 `0600` 的凭据文件中。

公有实例可使用细粒度 Token 环境变量：

```bash
export GITLAB_TOKEN=...
export GITHUB_TOKEN=...
```

也可以将 Token 存放在 `~/.bone/agent/forge-auth.json`。在 Unix 系统上，如果该文件允许 group 或 other 访问，Bone 会拒绝读取；文件权限必须为 `0600`。

```json
{
  "gitlab:gitlab.company.com": { "type": "token", "token": "$GITLAB_COMPANY_TOKEN" },
  "github:github.com": { "type": "token", "token": "$GITHUB_TOKEN" }
}
```

Token 会从远程错误中脱敏，也不会进入工具结果。应只授予实际操作所需的最小权限。

## 私有化 GitLab

所有非公有实例都必须在 `~/.bone/agent/forge.json` 中显式登记：

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

Bone 只接受 HTTPS API 地址，不跟随 API 重定向，并拒绝访问配置 API Host 之外的目标。唯一内置的跨 Host 映射是 `github.com` 到 `api.github.com`。

## 仓库策略

获得信任的仓库可以通过 `.bone/forge.yaml` 对 Bone 操作实施硬门禁：

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

未知字段和非法值会被拒绝。项目策略在项目获得信任前绝不会加载。合并门禁使用刚刚从远程读取的 MR 或 PR 状态；Bone 不信任模型参数中声明的 approval 或 pipeline 状态。

读取操作自动执行。敏感和破坏性写操作默认要求交互确认，并在 print、JSON 等非交互模式下拒绝执行。已完成的写操作按 `requestId` 记录；网络结果不明确时不会盲目重试。

## 工具

Plan Mode 可以使用 `forge_context`、`forge_query`、`forge_audit` 和 `forge_watch`。执行模式还提供 `forge_issue`、`forge_milestone`、`forge_change`、`forge_wiki`、`forge_pipeline`、`forge_release` 和 `forge_transition`。

`forge_context` 会逐项协商 GitLab 能力，而不会因为存在 `/api/v4` 就假设所有 API 都可用。能力结果区分 supported、unsupported、forbidden、disabled 和 unknown；高级能力缺失不会影响稳定的基础操作。

GitHub 没有正式支持的 Wiki REST API，因此 GitHub Wiki 写操作返回 `unsupported_capability`。GitHub REST 也不提供 review thread 的 resolved 状态；当策略要求所有讨论已解决时，GitHub 集成会按未满足处理。

## 兼容性

GitLab 版本、CE/EE、许可证、项目设置、feature flag 和 Token 权限都会影响 API 行为。Bone 会读取 `/api/v4/version`、逐资源探测能力、容忍新增响应字段并严格校验稳定标识，不根据版本号猜测端点。无法确认的能力会报告为 unknown，且不会用于受门禁保护的写操作。
