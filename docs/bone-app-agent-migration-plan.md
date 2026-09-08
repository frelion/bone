# bone-app 迁移到新 Agent API

状态：历史迁移笔记。2026-09-08 起，目标调整为 headless App 与独立 TUI 的完整重写，新的设计入口是 [bone-app 架构设计初稿](bone-app-design.md)。本文保留 Agent API 行为核对价值；其中保留 TUI 业务控制和旧 turn 结构的安排不再作为目标架构。

## 目标边界

`bone-app` 只做宿主适配、产品投影和持久化，不复制 Kernel 状态，也不保留旧 Agent API 的兼容层。运行对象只有 `Agent`；`AgentView` 提供某一时刻的完整结构状态，`Record` 提供此后的不可变事实流。

```text
bone-app ── post/control ──► Agent
    ▲                         │
    │                         ├── baseline: Arc<AgentView>
    └──── product reducer ────└── records: Receiver<Arc<Record>>
```

迁移必须保持四个边界：

1. 当前 Input 何时真正结束，何时只是等待用户；
2. 并行 Input 的回复与终态不会串到另一个命令；
3. 外部写何时已知，何时仍是 `Unknown`；
4. 增量流掉队后能从新 baseline 确定地重建。

Job 和 Call 的结束都不能替代 Input 的完成边界。

## 1. 启动与生命周期

删除 `AgentHost`、`AgentHandle`、`Runtime::spawn` 和分层 runtime config。普通启动直接使用：

```rust,ignore
let agent = Agent::start(
    workspace,
    coordinator,
    worker,
    agent_limits,
    tool_limits,
)?;
```

`AgentLimits` 从设置直接映射，使用 `Default` 加字段覆盖，不在 app 中再推导一份调度默认值。`ToolLimits` 只描述具体工具环境。

`Agent::start` 当前安装 `read`、`glob`、`grep` 三个只读工具。若 app 将来注入写工具，使用 `Agent::with_ports`，并由 `ToolPort::specification` 明确声明 `ToolEffect`；app 不从工具名称猜测效果。

每次创建 Agent 时，app 同时分配一个 durable `runtime_instance`。`Seq`、`CallId` 和 `JobId` 都只在该 Agent 进程实例内唯一；任何跨进程 journal key 都必须带 `runtime_instance`。

区分两个关闭入口：

- `stop()` 写入逻辑 Stop、Job Outcome、InputFinished 和 Stopped，Actor 仍可接收后续新输入；
- `shutdown()` 先 Stop，再等待 grace deadline，关闭 Actor，并返回 `ShutdownReport`。

session attachment 或进程退出走 `shutdown()`，用报告里的 unresolved writes 做最后一次持久化补漏。

## 2. 观察驱动

连接时先调用一次 `observe()`：

```text
Observation {
    baseline: Arc<AgentView>,
    after: Seq,
    records: broadcast::Receiver<Arc<Record>>,
}
```

先用 baseline 整体替换 UI 结构投影，再消费增量。驱动维护严格的连续序号：

```text
expected = observation.after

record.seq <= expected      忽略重复
record.seq == expected + 1  应用记录，expected = record.seq
其他 gap                    重新 observe，替换投影
Lagged                      重新 observe，替换投影
```

重新 observe 后丢弃旧 receiver，不能让旧流继续写入新 projection。UI baseline 可以整体替换；durable journal cursor 只能在对应 append 成功后推进，不能因为 UI reset 回退或越过未落盘记录。

`AgentView` 和 `Record` 用途不同：

- `InputView`、`JobView` 和 `CallView` 是 fresh baseline 中的当前结构状态；
- `Record` 是用户时间线、调用事实和持久化事件的增量来源；
- host 执行 pause、resume、cancel 等控制后，若界面显示精确 Job 树，应重新 observe；不要尝试只靠零散 Record 推演完整 Kernel 状态。

当前 `AgentView.calls` 只保留 Running、CancelRequested 和外部效果仍为 Unknown 的 Call。baseline reducer 将前两者放入 activity，将 Unknown 放入 attention；其余已结束历史来自 Record timeline。

## 3. Record reducer

删除 `Snapshot`、`StepEvent`、`RecordEntry`、`RecordKind`、`Notice` 等旧协议转换层，直接按新事实归约：

| 产品事实 | 新协议 |
| --- | --- |
| 用户原话 | `RecordBody::Input` |
| 可见中间回复 | `RecordBody::Reply` |
| 等待用户 | `RecordBody::Clarification` |
| 输入解释失败 | `RecordBody::InputRoutingFailed` |
| 模型或工具开始 | `RecordBody::CallStarted` |
| 最新调用进度 | `RecordBody::CallProgress` |
| 模型调用结束 | `RecordBody::CallFinished` |
| 工具真实结果 | `RecordBody::ToolFinished` |
| Job 契约变化 | `RecordBody::JobChanged` |
| Job 终态 | `RecordBody::Outcome` |
| Input 终态 | `RecordBody::InputFinished` |
| Stop 边界 | `RecordBody::Stopped` |

`RoutingStarted` 的 Routing ID、`Inquiry` 的 Inquiry ID 都是该 Record 自己的 `seq`。

Call reducer 只需四条规则：

```text
CallStarted(call)            打开 activity[call]
CallProgress(call)           替换 activity[call].progress
CallFinished(call)           关闭模型 activity
ToolFinished(call, outcome)  关闭工具 activity并更新该 call 的工具事实
```

工具调用的完整序列是 `CallStarted → ToolFinished`；工具没有第二个 `CallFinished`。`CallProgress` 只更新 activity，不增加 timeline 行。

`ToolFinished` 自带 `request` 与 `outcome`。工具名和参数读 `request`，结果或错误读 `outcome.result`，外部效果读 `outcome.external_effect`。终态渲染不需要回查旧 `CallStarted`。同一 `CallId` 的第二条 `ToolFinished` 是 Unknown 写的后续确证，更新同一项工具事实，不能显示成第二次执行。

## 4. 一个命令的生命周期

提交：

```rust,ignore
agent.post(Input::new(InputId(turn), text)).await?;
```

one-shot 等待器只处理与自己的 `InputId(turn)` 明确关联的记录：

- `InputFinished.input == InputId(turn)`：真正终态，按 Completed、Failed 或 Cancelled 返回；
- `Clarification.inputs` 包含当前 Input：本次产品调用以 WaitingForUser 返回；
- `InputRoutingFailed.inputs` 包含当前 Input：以可重试的 routing error 返回；
- `Reply.inputs` 包含当前 Input：可以作为本次中间输出显示，但不结束等待。

其他 Input 的 Reply、Clarification、RoutingFailed 或 InputFinished 必须忽略。任意 Job Outcome、模型 Call 结束和工具 Call 结束也不能提前结束当前命令。

等待用户时保存“本次处于 WaitingForUser 的 InputId”。用户回答创建新 ID，并明确引用它：

```rust,ignore
Input::new(next_input_id, answer)
    .replying_to(waiting_input_id)
```

多轮澄清时 `waiting_input_id` 可能是上一次回答，不一定是会话最初的 Input。不要取 `Clarification.inputs.first()`，也不要从自然语言推断关联。Kernel 已全局串行化用户问题，app 无需再实现 per-job clarification queue。

Clarification 后旧 Kernel Input 仍未终态；工作完成时，它可能晚于后续回答收到自己的 `InputFinished`。reducer 永远按记录内的 InputId 找对应 turn。旧 turn 的迟到终态不能结束当前回答 turn。

## 5. Durable journal

### 澄清

把“显示问题”和“本轮返回 WaitingForUser”写成一条原子事实：

```rust,ignore
ClarificationRequested {
    runtime_instance: u64,
    record_seq: u64,
    turn: u64,
    input: u64,
    question: String,
}
```

以 `(runtime_instance, record_seq)` 去重，以 `turn` 更新产品生命周期。重放该事实会恢复问题、结束对应 active turn，并把 session 置为 WaitingForUser；它不会把 Kernel Input 标成 finished。

### 外部写

durable key 使用 `(runtime_instance, CallId)`：

```rust,ignore
ExternalWriteUnresolved { runtime_instance, call, tool }
ExternalWriteResolved   { runtime_instance, call }
```

Journal 保持 append-only，reducer 以同一 key 的最新事实决定 attention 状态：

```text
ToolFinished(call, Unknown)  追加 Unresolved
ToolFinished(call, known)    追加 Resolved
```

同一 live Agent 中，`resolve_write(...).await == Ok(())` 只表示控制命令已受理。只有观察到后续同 CallId 且 `outcome.external_effect != Unknown` 的 `ToolFinished`，才能追加 Resolved。

冷启动后的旧 Agent 已不存在，旧 CallId 不能传给新 Agent 的 `resolve_write`。恢复出的 unresolved effect 由产品的人机确认流程直接追加 durable resolution；不能冒充旧 Kernel 已恢复执行。`ShutdownReport.unresolved_writes` 只有 call、job、tool，用于关机补漏，不替代完整工具结果。

`bone-agent` 当前不承诺跨进程恢复执行中的 Job。冷启动恢复用户可见会话与 unresolved effect；丢失的运行工作应显示为 interrupted，不能伪装成完成。

## 6. TUI projection

保留现有多 session、独立草稿、焦点、未读和布局逻辑，只替换它们的数据入口：

- `AgentView`：首次加载、控制后的状态刷新、sequence gap 或 Lagged 后重建；
- `Record`：时间线和 activity 的增量 reducer；
- `InputView.status`：一条用户输入的当前阶段；
- `JobView.status`：工作树与诊断，不直接生成用户消息；
- `CallView`：当前运行 activity 与 Unknown attention。

慢 journal writer 不应阻塞 Agent Actor。driver 可以先在自己的有界队列中排序，再由单 writer 按 Seq 追加；队列满时停止推进 durable cursor并触发明确重建，不能静默跳过事实。

测试 fixture 只需要 `record(seq, origin, body)` 和一个最小 `AgentView` 构造函数。不要重建 `LegacySnapshot`、`TestStepEvent` 或 `From<OldType>`。

## 7. 实施顺序

1. 迁移 settings、provider 和 startup，使 app 能创建一个 `Agent`，并分配 `runtime_instance`。
2. 将 runtime driver 改为 baseline 加连续 Record 流，实现 duplicate、gap 与 Lagged 重建。
3. 用 `RecordBody` 重写 timeline/activity reducer，先覆盖 Input、Reply、Clarification、RoutingFailed、Call 和 Tool。
4. 迁移 one-shot 的 InputId 过滤与三个结束边界。
5. 迁移 journal，先保证 Clarification 原子事实，再实现 Unknown → known 的 append-only 确证。
6. 接入 fresh `AgentView` 的 Job/Input 诊断状态；最后删除旧协议类型、兼容转换和旧 fixture。

## 8. 验证范围

只在 `bone-app` 保留三层宿主测试，不复制 `bone-agent` 的状态机测试。

**Reducer：**

- baseline 正确恢复 running、cancel-requested 与 Unknown，历史已完成 Call 不会重新显示成 activity；
- CallStarted/Progress/CallFinished 和 CallStarted/ToolFinished 各归约一次；
- Unknown 后第二条 known ToolFinished 只解析同一个 `(runtime_instance, CallId)`；
- Reply、Clarification、RoutingFailed 和 InputFinished 都按 InputId 过滤；
- 前一轮 Clarification 的迟到 InputFinished 不结束当前回答 turn。

**Runtime driver：**

- baseline 总在增量之前；重复 Record 被忽略；sequence gap 与 Lagged 都替换 receiver 和 projection；
- routing failure 不让 one-shot 永久等待；
- 工具等待在 ToolFinished 结束，不等待不存在的工具 CallFinished；
- 慢 UI 或日志消费者不反压 Agent。

**Durable session：**

- Clarification 的问题与 WaitingForUser 边界一次原子落盘；
- runtime ID 防止重启后的 CallId/Seq 碰撞；
- Unknown 只有在第二条 known ToolFinished 后解除；
- shutdown report 能补录遗漏的 unresolved write；
- runtime 启动失败保留 durable pending post，冷恢复不会重放未确认输入。

并发调度、Worker 上下文隔离、pause 权限、子树取消、工具真实终态和 checkpoint 全部留在 `bone-agent` 测试。完成迁移后运行 `bone-app` 的 reducer、runtime-driver、durable-session 测试，再运行 package 全测试。
