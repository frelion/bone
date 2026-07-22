# Agent Tool Contract v1

[English](../agent-tool-contract.md) | [简体中文](agent-tool-contract.md)

Bone 面向模型的工具采用 Agent Tool Contract v1，以提升首次调用正确率、错误恢复能力，并限制上下文占用。除了发送给模型的 JSON Schema，`ToolDefinition.contract` 还提供机器可读的行为元数据。

## 输入 Schema

- 每一层对象都使用 `additionalProperties: false` 的封闭 Schema。
- 互斥操作必须建模为带判别字段的 union，不得暴露一个宽松参数包，再根据字段是否存在猜测操作。
- 判别字段和该操作必需的字段必须显式提供；不适用的字段直接省略。
- Schema 应拒绝空字符串、值为零的标识、空数组、`null` 占位符和无上限集合。
- 使用 `issueNumber`、`changeNumber`、`pipelineId` 等领域名称；平台专用字段在工具边界之后转换。
- Contract 应包含少量正确示例。描述用于强化 Schema，不能替代 Schema。

## 结果与错误契约

工具返回稳定、面向任务的投影，不返回平台或底层库的原始响应对象。每个 Contract 声明默认和最大字节预算，也可以声明条目预算。大型字段通过有界预览或显式详情操作获取；发生截断时仍返回合法结构化数据，并说明省略内容。

执行错误应提供稳定错误码、简洁消息和 `retryable: true` 或 `retryable: false`。参数校验错误只包含有界且 UTF-8 安全的输入预览。确定性校验错误或不可重试错误发生后，Agent loop 会拒绝完全相同的再次调用；参数已修正，或上次属于瞬时错误时，仍允许重试。

扩展工具应抛出 `AgentToolError`，不要把错误编码成成功结果，也不要手工拼接 JSON：

```ts
throw new AgentToolError("rate_limited", "平台冷却后可以重试", true, {
  retryAfterSeconds: 30,
});
```

模型只能看到有界的恢复元数据：`retryAfterSeconds`、`statusCode`、`provider`、`resource`、`operation`、`field` 和 `requestId`。平台原始响应、请求头、凭据及任意嵌套对象必须留在受保护日志中；未识别的详情字段会被省略。

Agent loop 会将其转换成统一的结构化错误 envelope，并执行工具的 retry policy。成功调用会重置 retry chain；fingerprint 使用准备并校验后的参数，因此不同的原始写法只要规范化成相同调用，就不能绕过确定性失败保护。

## 副作用与幂等

每个 Contract 将工具标记为 `read`、`routine_write`、`sensitive_write` 或 `destructive`，同时声明幂等是内在保证、必须提供还是不可用，并列出允许在有限次数内重试的错误码。

副作用元数据不能替代运行时授权。仓库信任、策略门禁、交互确认、凭据和远程权限始终在实际执行时验证。

## 定义示例

```ts
const contract = defineAgentToolContract({
  version: 1,
  useWhen: ["读取当前仓库的一条 issue"],
  doNotUseWhen: ["issue number 未知"],
  effect: "read",
  idempotency: "inherent",
  retry: {
    retryableErrors: ["rate_limited", "remote_failure"],
    maxAttempts: 2,
    rejectUnchangedRetry: true,
  },
  outputBudget: { defaultBytes: 16 * 1024, maximumBytes: 64 * 1024 },
  examples: [{ description: "读取 issue 42", input: { operation: "get", id: 42 } }],
});
```

Contract 变更必须通过 Agent 实际使用的同一套校验与执行路径，覆盖合法调用、污染或歧义调用、确定性错误恢复、瞬时错误重试、结果投影和 UTF-8 字节限制。
