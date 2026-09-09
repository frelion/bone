# App：headless 产品后端

`bone-app` 是 BONE 的 composition root。它把 `bone-core`、`bone-adapters`、配置、凭据和持久化装成 Workspace / Session 产品 API，供 TUI、桌面、Web 或自动化前端共同使用。

App 不决定如何拆分 Job、如何构造 Worker Context 或何时允许 Job 完成；这些属于 Core。它也不重新实现模型协议和本地工具；这些属于 Adapters。

```text
frontend
   │  App / Session API
   ▼
bone-app
   ├── config + profiles + credentials
   ├── private SQLite storage + leases
   ├── Session tasks + Runtime lifecycle
   ├── write tracking + durable history
   ├── bone-adapters ── ModelPort / ToolPort
   └── bone-core ───── Agent
```

## 对象、所有权与身份

`App` 持有一份私有 Store、provider connector、Workspace 写入 gate 和所有已打开 Session 的注册表。一个 App 可以管理多个 Workspace 和 Session。

`Session` 是前端长期持有的克隆句柄。每个已打开 Session 对应一个串行 `SessionTask`，它按顺序处理提交、控制、配置切换、历史归档和 Runtime 收尾；Agent 内的多个 Job 仍可并发。相同 App 多次打开同一 Session 会取得同一任务。释放前端句柄或观察 receiver 不会停止工作。

身份分为三层：

| 身份 | 范围 |
| --- | --- |
| `WorkspaceId`、`SessionId` | 稳定产品身份，跨进程重启保留 |
| `RequestId`、`InputId`、`SessionSeq` | Session 内的提交幂等、输入身份和耐久历史位置 |
| `RuntimeId` + local Job / Call / Record ID | 一次 Agent Runtime 内的执行身份 |

`JobRef`、`CallRef` 和 `QuestionId` 都携带 `RuntimeId`。Session 在控制、回答或写核查时先核对 Runtime，旧界面的引用不能影响替换后的 Runtime。

## 启动与公共 API

宿主选择一个绝对数据目录：

```rust,no_run
use bone_app::{App, AppOptions, SessionSeq, SubmitInput};

# async fn run() -> bone_app::Result<()> {
let app = App::open(AppOptions::new("/var/lib/my-bone")).await?;
let workspace = app.open_workspace("/workspace/project").await?;
let session = app.create_session(workspace.id, "Investigate CI").await?;

let receipt = session.submit(SubmitInput::new("Find the failure")).await?;
let history = session.history(SessionSeq(0), 32).await?;

# let _ = (receipt, history);
app.shutdown().await?;
# Ok(())
# }
```

`open_workspace` 要求目录已经存在，并使用其 canonical path 取得或创建稳定 Workspace。它不向上猜 Git root，也不在项目中创建 `.bone/`。Session 创建后永久绑定自己的 Workspace。

App 的主要职责入口：

- Workspace / Session：`open_workspace`、`create_session`、`session`、`list_sessions`。
- 配置：`config`、`update_config`、`resolved_config`。
- provider：`profiles`、`save_profile`、`set_api_key`、`login`、`logout`。
- 安全收尾：`unresolved_writes`、`shutdown`。

Session 的主要职责入口：

- 执行：`submit`、`retry`、`control`、`stop`、`reload_config`、`close_runtime`、`resolve_write`。
- 读取：`snapshot`、`observe`、`history`。
- 产品状态：`rename`、`save_draft`、`archive`。

精确签名和 DTO 字段以 [crate root](../crates/bone-app/src/lib.rs) 与 rustdoc 为准。App API 不暴露 SQLite、Document key、journal transaction、Agent handle 或 provider client。

## 输入的耐久边界

`Session::submit` 的成功含义很窄：输入、调用方提供的 `RequestId` 映射和 `InputSubmitted` 事件已经在同一事务中提交。它不表示模型已经启动或 Input 已完成。

```text
SubmitInput
   │
   ├── transaction: input + request index + InputSubmitted
   │       └── commit ──► SubmissionReceipt
   │
   └── SessionTask 按队列尝试投递给当前/新 Runtime
              └── Agent facts 持久化后再发布 SessionView
```

相同 `RequestId` 与相同正文、回答引用重复提交会返回原回执；同一 ID 对应不同内容返回 `RequestConflict`。调用方取消等待不会撤销已经进入 SessionTask 的提交。输入和草稿各自最多 1 MiB UTF-8，在写入任何 durable envelope 前校验。

输入状态表达保存与执行之间的真实差别：

```text
Queued(problem?) → Posting(runtime) → Accepted(runtime)
       │                                  ├── WaitingForUser
       │                                  ├── RoutingFailed
       │                                  └── Finished(outcome)
       ├── Rejected
       ├── Cancelled
       └── Interrupted(runtime)
```

- 缺少模型、配置无效、登录未就绪、provider 启动失败或 Core 暂时 Busy 时，已保存输入保持 `Queued`，Session 按顺序重试。
- Agent 明确拒绝一个已保存输入时，App 记录 `Rejected`；不会让它永久停在队列中。
- `WaitingForUser` 返回完整 `QuestionId`。前端把它原样放回 `SubmitInput::answer`，App 与 Core 共同验证问题仍然有效。
- `retry` 只用于普通 Queued 或同一 Runtime 的 `RoutingFailed`。已经 Finished / Interrupted 的需求通过新 Submit 表达。
- `stop` 与 submit、retry、Job control 使用同一 Session 命令顺序：它取消此前排队和已经投递的工作，此后到达的提交可以开始新工作。

Session command channel 有界。已经持久化的 Queued 输入当前没有单独的总量上限；Core 对待处理 Input 和活跃 Job 有自己的容量限制。

## Session 当前状态与历史

`SessionView` 是前端可以直接渲染的当前投影，包括：

- Session 元数据、草稿和 Runtime 生命周期；
- 尚需关注的 Input、Job 和 Call activity；
- 已持久化历史的 `history_through`；
- 一个可模式匹配的 `AppProblem`。

`RuntimeState` 只有 `Detached`、`Starting`、`Running` 和 `Closing`。前端只依据这个公开状态判断生命周期，不推断 SessionTask 的内部任务或 handle。

`observe` 使用 Tokio `watch`，立即返回最近 View，随后只通知“状态已经变化”。watch 允许合并通知，不是事件队列。`snapshot` 通过 SessionTask 取得一次新鲜 View。

`history(after, limit)` 从 SQLite journal 返回公开 `SessionEvent`，包含下一游标和 `has_more`；当前每页最多 32 项。公开事件包括输入保存/接受/拒绝/完成、问题、回复、Job 终态、工具终态、Runtime 生命周期、中断和写核查。Agent progress 可以进入内部 journal 以保持归档水位连续，但公开 history 过滤它，View 只保留最新 activity。

前端的无丢失读取顺序固定为：

1. 先调用 `observe`，保存 receiver 当前的 `history_through`。
2. 用 `history` 从已有 cursor 补读到该水位。
3. 每次 watch 变化后更新 View，并继续分页到新的水位。
4. 断线或 receiver 合并通知时仍从最后的 `SessionSeq` 继续。

SessionTask 观察 Core 时采用同一原则：broadcast 只负责唤醒；它重新取得 `AgentView`，按 Agent Seq 筛选未归档 Record，在持久化事务成功后才推进水位和发布 App View。存储失败不会发布虚假的“已保存完成”。

## 配置与运行中生效

配置只有三个类型化作用域：

```text
Session override > Workspace override > User setting
```

`RuntimeOverrides` 包含 Worker、Coordinator、`AgentLimits` 和 `ToolSettings`。没有 Coordinator override 时它跟随最终 Worker；没有任何 Worker 时，resolved desired 为 `ConfigProblem::NeedsModel`。这不会阻止打开 Workspace、Session、历史或草稿。

`ConfigChange` 每次只修改一个字段。传入 `None` 清除本作用域 override；User 的 limits / tools 清除后回到类型默认值。配置保存使用最新 revision 更新指定字段，避免两个独立字段的修改互相覆盖。

`App::update_config(scope, change).await` 是当前 App 进程内的生效屏障：

1. 校验并持久化 desired override；
2. 找出受影响的所有已打开 Session；
3. 每个 Session 阻止新调度并解析完整 `RuntimeConfig`；
4. 对 Running Agent 原子更换模型、工具和 limits，先持久化 `RuntimeReconfigured`，再恢复调度；
5. 等待所有目标 Session 返回确认。

运行中配置成功时保留 `RuntimeId`、Job 图和在途工具。旧模型调用失去提交资格并用新模型重新调度；已经开始的工具使用启动时捕获的端口和 limit 收尾。

如果 desired 配置无法装配，保存值不回滚，旧 running config 仍可查询，但 Agent 保持 suspended，不再启动新模型或工具。修复配置后再次 `update_config`，或修复凭据后调用 `Session::reload_config`，会在同一 Job 图上重试。fan-out 不是跨 Session 回滚事务：已经成功应用的 Session 不因另一 Session 失败而倒退。

调用方取消 `update_config` / `save_profile` Future 只停止等待，不撤销已经交给 App 的变更。后续配置操作、`resolved_config` 与 shutdown 会在同一屏障后继续。

这个生效保证只覆盖同一 App 实例。没有跨进程配置 watcher；另一个进程在下次解析或执行自己的配置操作时读取 durable 值。

## Profiles、凭据与模型连接

`Profile` 只保存稳定 `ProfileId`、显示名和非 secret 的 `EndpointConfig`。支持：

- ChatGPT subscription；
- OpenAI Responses official / HTTPS compatible；
- OpenAI Chat Completions official / HTTPS compatible；
- Anthropic Messages official / HTTPS compatible。

App 层只允许 HTTPS compatible URL，禁止把凭据嵌入 base URL。模型选择保存 profile ID、model ID 和与 endpoint protocol 匹配的类型化 `ModelOptions`。

API key 保存在操作系统 credential manager，slot 同时绑定 profile 与 endpoint identity；改变 endpoint 不会把旧 key 静默发送到新服务。API key 不实现可泄漏内容的 `Debug` / `Display`，也不进入 SQLite、历史或模型上下文。

ChatGPT subscription 使用 provider 管理的 OAuth cache。App 持有对 cache 的互斥 lease，但不解析或复制其 JSON。普通 Runtime 启动只尝试 cached auth；缺少登录时暴露 `LoginRequired(ProfileId)`，不会自行弹出设备流程。

`App::login` 显式返回 `LoginAttempt`，其 watch 状态为 Connecting、DeviceCode、Succeeded、Failed 或 Cancelled。丢弃或调用 `cancel` 会结束本次交互等待。登录、连接、logout 和 App shutdown 在同一个 provider operation 边界协调；冲突返回 `ProfileBusy`。live Runtime 仍持有凭据能力时，logout 不会删除 cache。

## Runtime 装配与旧会话背景

首次有 Queued 输入需要执行时，Session 惰性创建 Runtime。启动流程是：

1. 读取 User / Workspace / Session overrides 与 Profile；
2. 取得 API key 或 ChatGPT cached auth 能力；
3. 构造 Worker / Coordinator `ModelAdapter`；
4. 以 canonical Workspace root 和 `ToolSettings` 构造工具；
5. 从 durable public history 中选取预算内的近期 `BootstrapContext`；
6. 创建 `Agent::with_ports_and_background`，持久化 RuntimeStarted 后投递输入。

App 不恢复旧 Runtime 的 future、Job ID 或工具调用。bootstrap history 是受限的只读材料；完整旧历史可由内置 `session_history` 工具按 `SessionSeq` 读取。该工具每次扫描一个耐久位置。如果单个公开事件超过 Runtime 的工具输出预算，它返回 `omitted: true` 并推进 cursor，不截断存储事实，也不让后续页永远不可达。

当前 bootstrap history 从 Session journal 起点线性扫描，再保留预算内的近期事件。它正确但随长会话增长；倒序索引或摘要属于后续性能工作。

## 写工具与外部效果

`ToolMode::ReadOnly` 安装 `read`、`glob`、`grep` 和 `session_history`。`WorkspaceWrite` 另外安装受 App 包装的 `apply_patch` 与 `bash`。

一次写调用的顺序是：

```text
Workspace gate
  → 检查没有未解决写
  → durable begin_write(Pending)
  → 执行工具
  → durable finish_write(outcome)
  → Core 收到 ToolOutcome
  → 匹配的 Agent Record 归档
  → 删除 hot unresolved entry
```

同一 App 内，同一 Workspace 的写调用使用一个异步 gate 串行。不同进程、不同 Session 对同一 Workspace 的写入目前没有全局互斥保证；部署方式必须限制为一个执行所有者，或在宿主层补充隔离。

写任务持有 Session lease，即使 Agent shutdown grace 已到也不会让另一个进程在旧任务仍可能改文件时打开同一 Session。工具返回 `Unknown`、App 在执行后无法保存结果、或进程在 durable acknowledgement 前退出时，该写保持 unresolved，并阻止后续 Workspace 写。

`App::unresolved_writes(workspace)` 是跨 Session 权威查询。`Session::resolve_write` 接受对应 `CallRef`、明确的 `None` / `Applied` 和核查证据；它更新同一次工具事实，不制造第二次执行。相同确认幂等，冲突确认报错，仍在运行的写返回 `WriteInProgress`。

## 持久化、并发与恢复

App 在 `<data_dir>/bone.sqlite3` 和私有 `leases/` 下保存自己的数据。SQLite 包含：

- canonical Workspace identity；
- Session metadata、draft、input、RequestId index；
- User / Workspace / Session 配置与非 secret Profile；
- Runtime config snapshot、原始 Agent Record 和公开 Session journal；
- 外部写入意图、结果和后续核查。

私有 storage 模块拥有 schema、document CAS、journal、事务、SQLite 连接和 OS lease。损坏、权限不安全或未知 schema 直接返回错误；App 不自动 reset、删除或猜测迁移。SQLite 用一条 App writer 连接串行短事务，WAL 允许并发读；独立连接使用有界 busy timeout。

每个打开 Session 取得一个跨进程 writer lease，第二个进程打开同一 Session 会得到 `SessionBusy`。archive 只改变组织状态，不隐式取消或关闭 Runtime。

进程重启时：

- stale Runtime 在存储中关闭；
- 已经投递但未终态的 Input 标为 `Interrupted`；
- 能证明尚未投递的 Input 保持 Queued；
- Session metadata、draft、history、配置和 unresolved writes 恢复；
- 外部调用和 Job future 不恢复，未知写不自动重放。

`close_runtime` 只收尾 Agent 并保留 Session；稍后提交可以创建新 Runtime。`App::shutdown` 关闭所有 Session Runtime 和 provider 操作，清空进程内注册表，返回仍未解决写入。多次调用在完成后返回同一报告。

## 错误与前端契约

公开 `Error` 和 `AppProblem` 提供前端可匹配的边界，包括 Closed、Workspace / Session 不存在、SessionBusy、RequestConflict、StaleRuntime、WriteInProgress、InvalidState、Configuration、LoginRequired、ProfileBusy、Provider、Storage、Tools 和 Agent。

`SessionView::problem` 用于异步启动、归档或配置失败。前端应匹配枚举并提供恢复动作；字符串消息用于诊断，不应成为状态判断协议。

## 代码入口

- [`api.rs`](../crates/bone-app/src/api.rs)：前端 DTO、身份和回执。
- [`app.rs`](../crates/bone-app/src/app.rs)：composition root、配置屏障与 provider 生命周期。
- [`session.rs`](../crates/bone-app/src/session.rs)：Session actor、Runtime 装配、观察与收尾。
- [`config.rs`](../crates/bone-app/src/config.rs)：三层解析、验证和 RuntimeConfig。
- [`persistence.rs`](../crates/bone-app/src/persistence.rs)：产品持久化语义。
- [`storage/`](../crates/bone-app/src/storage/)：私有 SQLite、journal、CAS 和 lease。
- [`providers.rs`](../crates/bone-app/src/providers.rs) 与 [`tools.rs`](../crates/bone-app/src/tools.rs)：具体装配、凭据能力和写跟踪。

Core 的执行不变量见 [Core](core.md)，模型和原生工具边界见 [Adapters](adapters.md)，TUI 接入顺序见 [TUI](tui.md)。
