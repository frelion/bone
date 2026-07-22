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

Forge 工具遵循 [Agent Tool Contract v1](agent-tool-contract.md)。`forge_context` 的参数只能是 `{}`，仓库和平台直接根据当前 Git 工作区解析。`forge_query` 必须显式选择一种操作：

```json
{"operation":"list","resource":"issue","state":"open","limit":10}
{"operation":"get","resource":"issue","id":42}
{"operation":"get_many","resource":"issue","ids":[42,43]}
```

不要发送无关字段或空占位符。写工具同样采用按 action 区分的封闭 Schema，并使用领域标识。例如 issue 评论使用 `issueNumber`，不使用通用 `id`：

```json
{"action":"comment","requestId":"issue-42-comment-1","issueNumber":42,"input":{"body":"已通过回归测试确认。"}}
```

`requestId` 表示一次写操作意图，只能在重试完全相同的意图时复用。GitHub 与 GitLab 的字段差异由内部转换。

`forge_query` 返回紧凑摘要，不会返回平台原始响应。列表默认返回 10 条，最多 50 条，每条最多包含 384 字节正文预览。使用 `search` 执行平台侧的当前仓库文本检索，使用 `cursor` 获取下一页。单条详情正文限制为 16 KiB，批量详情每条正文限制为 8 KiB；列出某个 pipeline 或 workflow run 的 jobs 时使用 `parentId`。所有 Forge 工具结果还具有独立的 64 KiB 输出上限，并在省略内容时返回明确的截断元数据。非法参数的回显也受到限制，失败调用不会把任意大的 payload 重新写入 Agent 上下文。

`forge_context` 会返回紧凑的平台身份信息，并逐项协商资源能力，而不会假设实例开放了所有端点。能力结果区分 supported、unsupported、forbidden、disabled 和 unknown。`supported` 仅表示当前 Token 可以访问读取端点，不保证拥有创建、修改、合并、重跑或发布权限；写权限由实际写请求和策略门禁验证。高级能力缺失不会影响稳定的基础操作。

GitHub 没有正式支持的 Wiki REST API，因此 GitHub Wiki 写操作返回 `unsupported_capability`。GitHub REST 也不提供 review thread 的 resolved 状态；当策略要求所有讨论已解决时，GitHub 集成会按未满足处理。

## 离线协议评估

Forge 提供一个确定性、无网络的 scripted 评估 harness。它运行真实 Agent loop、真实 Forge schema 和 wrapper，并使用严格匹配的 Fake `ForgeService`；不会使用 token，也不会访问 GitHub/GitLab。案例覆盖正常查询、schema 修正、紧凑 mutation receipt、不可重试错误后的修正、有限重试、大结果预算和写操作顺序。

在仓库根目录运行：

```bash
npm run eval:forge
```

命令会生成 `.artifacts/forge-eval/report.json` 和 `.artifacts/forge-eval/report.html`。JSON 是机器读取的规范结果，HTML 用于查看每个案例的工具调用、fake service 调用、上下文观察和硬断言。CI 也会运行同一个离线命令，并将两个文件作为 `forge-protocol-eval` artifact 上传。通过报告表示 Forge 协议不变量成立；它不代表通用模型智能、生产延迟或 GitHub/GitLab 版本兼容性。后面三项需要单独的真实模型回放和 provider canary 评估。

## 兼容性

### 真实模型评估（显式 opt-in）

使用 `npm run eval:forge:live -- --model provider/model --runs 3` 运行真实模型评估。模型通过 Bone 的 `ModelRuntime` 获取，因此沿用 `/settings`、`models.json`、`auth.json`、OAuth 和自定义 provider 配置；未传 `--model` 时使用设置中的默认模型或第一个已认证模型。评估仍使用 Fake Forge Service，不会访问或修改真实 GitHub/GitLab，默认输出到 `.artifacts/forge-eval/live`。

可用 `--max-turns` 和 `--timeout-ms` 限制成本与运行时间。报告单独使用 `taskCompletionRate`、首个工具选择正确率、首调参数合法率、纠错成功率、确定性重复率、平均工具调用数和平均上下文字节；这些指标与 scripted 的 `protocolPassRate` 不合并，也不能单独证明新旧契约的因果改进。要做 baseline/candidate 对比，必须固定同一模型、提示、fixture 和 runs，在旧契约与当前契约上分别生成报告。

GitLab 版本、CE/EE、许可证、项目设置、feature flag 和 Token 权限都会影响 API 行为。Bone 会读取 `/api/v4/version`、逐资源探测能力、容忍新增响应字段并严格校验稳定标识，不根据版本号猜测端点。无法确认的能力会报告为 unknown，且不会用于受门禁保护的写操作。
