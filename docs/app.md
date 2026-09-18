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
let app = App::open(AppOptions::with_paths(
    "/var/lib/my-bone/data",
    "/var/lib/my-bone/home",
)).await?;
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
       │                                  ├── ConversationFailed
       │                                  └── Finished(outcome)
       ├── Rejected
       ├── Cancelled
       └── Interrupted(runtime)
```

- 缺少模型、配置无效、登录未就绪、provider 启动失败或 Core 暂时 Busy 时，已保存输入保持 `Queued`，Session 按顺序重试。
- Agent 明确拒绝一个已保存输入时，App 记录 `Rejected`；不会让它永久停在队列中。
- `WaitingForUser` 返回完整 `QuestionId`。前端把它原样放回 `SubmitInput::answer`，App 与 Core 共同验证问题仍然有效。
- `retry` 只用于普通 Queued 或同一 Runtime 的 `ConversationFailed`。已经 Finished / Interrupted 的需求通过新 Submit 表达。
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

`history(after, limit)` 从 SQLite journal 返回公开 `SessionEvent`，包含下一游标和 `has_more`；当前每页最多 32 项。公开事件包括输入保存/接受/拒绝/完成、问题、回复、Job 创建/终态、模型与工具调用生命周期、Runtime 生命周期、中断和写核查。调用开始事件只公开类型、Job / Call 身份和工具名，不公开工具参数。Agent progress 可以进入内部 journal 以保持归档水位连续，但公开 history 过滤它，View 只保留最新 activity。

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

持久化边界与作用域一致：User settings 和所有 Profiles 写入
`$BONE_HOME/config.toml`，Workspace override 写入 `<workspace>/.bone/config.toml`，只有
Session override 留在 SQLite。`BONE_HOME` 必须是绝对路径；平台默认值为 `~/.bone`。
项目文件只能引用用户级 Profile ID，不能声明 Profile、Endpoint、凭据路径或 secret。
两个 TOML 文件都带 `schema_version = 1`，未知字段和未知版本会被拒绝。

`RuntimeOverrides` 包含 Worker、Coordinator、`AgentLimits` 和 `ToolLimits`。没有 Coordinator override 时它跟随最终 Worker；没有任何 Worker 时，resolved desired 为 `ConfigProblem::NeedsModel`。这不会阻止打开 Workspace、Session、历史或草稿。

`ConfigChange` 每次只修改一个字段。传入 `None` 清除本作用域 override；User 的 limits / tools 清除后回到类型默认值。文件写入在 App-owned advisory lock 内重新比较载入时的 SHA-256 摘要，因此并发 BONE 进程或外部编辑不会静默覆盖；成功写入使用同目录临时文件、同步和原子替换。项目 `.bone` 必须是真实目录，配置文件也不能是符号链接。

配置只在 App 启动、打开 Workspace 或显式 reload 时读取，不使用 watcher。
`reload_config(workspace)` 先完整解析并校验 User 与 Workspace 两个文件，再一起替换内存
快照，并只重配一次已打开 Session；任一文件失败时两个快照都不变。返回的
`ReloadConfigOutcome` 会明确给出项目文件是否存在、是否仍受信任。细粒度 embedder 仍可用
`reload_user_config` / `reload_workspace_config`。TUI 的 `/reload-config` 使用组合接口。

项目配置在打开工作区或执行 `/reload-config` 时解析并生效。配置文件使用严格 schema
校验；写入时通过文件锁和摘要检查避免覆盖外部修改。

`App::update_config(scope, change).await` 的成功边界是保存并通知：

1. 校验并持久化 desired override；
2. 找出受影响的所有已打开 Session；
3. 向这些 Session 排入配置变更通知并返回，不等待 provider 或网络。

Session 收到通知后解析完整 `RuntimeConfig`。已有 Runtime 会先暂停新调度，再在独立任务中装配模型；actor 仍可处理 stop、close 和更新的配置通知。只有最新 desired 配置可以安装：它原子更换模型、工具和 limits，先持久化 `RuntimeReconfigured`，再恢复调度。显式 `Session::reload_config` 才等待运行中 Runtime 完成重配。

运行中配置成功时保留 `RuntimeId`、Job 图和在途工具。旧模型调用失去提交资格并用新模型重新调度；已经开始的工具使用启动时捕获的端口和 limit 收尾。

如果配置无法装配，保存值不回滚，旧 running config 仍可查询，但 Agent 保持 suspended，不再启动新模型或工具。`ConfigChange::Model` 只校验静态的 profile/model 关系，不读取 OAuth cache；缺少登录会由 Session 异步暴露为 `LoginRequired`。修复配置后再次 `update_config`，或修复凭据后调用 `Session::reload_config`，会在同一 Job 图上重试。API key 更新会强制刷新所有实际引用该 profile 的已打开 Session。

调用方取消 `update_config` Future 不撤销已持久化的变更。`resolved_config` 同时返回 durable desired 与当前 running config，前端可直接呈现两者短暂不一致的应用过程。`save_profile` 与 App 级 reload 同样在保存并通知后返回；只有显式的 `Session::reload_config` 等待该 Session 的运行中 Runtime 完成应用。

这个生效保证只覆盖同一 App 实例。没有跨进程配置 watcher；外部修改要到显式 reload
或下一次 App 启动才会载入，写操作发现摘要变化则返回冲突。

## Profiles、凭据与模型连接

`Profile` 只保存稳定 `ProfileId`、显示名和非 secret 的 `EndpointConfig`。支持：

- ChatGPT subscription；
- OpenAI Responses official / HTTP or HTTPS compatible；
- OpenAI Chat Completions official / HTTP or HTTPS compatible；
- Anthropic Messages official / HTTP or HTTPS compatible。

兼容服务 URL 必须是绝对 HTTP(S) 地址，不能包含用户名、密码或 query；请求不跟随重定向。
Profile 同时保存手动添加的模型 ID 列表，模型选择保存 profile ID、model ID 和与 endpoint
protocol 匹配的类型化 `ModelOptions`。HTTP 地址是否可信由配置使用者负责。

API key 只保存在 `$BONE_HOME/credentials.toml`。目录在 Unix 上要求 `0700`，文件要求
`0600`，且拒绝符号链接、额外硬链接和非普通文件；更新使用文件锁、私有临时文件、同步
和原子替换。每项绑定 Profile ID 与“协议 + 规范化 base URL”的 SHA-256 Endpoint 指纹，
改变协议或地址后旧 key 不会被发送到新服务。API key 不实现可泄漏内容的 `Debug` /
`Display`，错误也不包含 secret，并且它不进入配置文件、项目目录、SQLite、历史或模型上下文。
自动化可将 key 通过 stdin 交给 `bone credentials set`；该命令复用同一 Rust 后端，
不在 shell 参数或 `bone run` 环境中传递 secret。

ChatGPT subscription 使用 `$BONE_HOME/providers/chatgpt-subscription/auth.json` 与
`auth.lock`。App 只验证并传递私有 cache 路径，不解析或复制其 JSON，也不把它与 API-key
文件混合。构造 Endpoint 是纯本地操作；真正的模型请求由 Rig 读取、刷新并提交 cache。
缺少登录时暴露 `LoginRequired(ProfileId)`，不会自行弹出设备流程。

`App::login` 显式返回 `LoginAttempt`，其 watch 状态为 Connecting、DeviceCode、Succeeded、Failed 或 Cancelled。丢弃或调用 `cancel` 会结束本次交互等待。普通 Runtime 不会自行启动交互登录；logout 直接参与下面的 cache 事务。

Rig 为每次 cache 事务取得跨进程文件锁：锁内重新读取记录，按需完成 refresh，并以同目录临时文件原子替换；invalidate 与 logout 使用同一把锁。等待设备授权的人机阶段不持锁，成功提交时再取锁。锁等待和 HTTP 请求都有界且取消安全，因此一个 App 不再以生命周期 lease 排斥另一个 App。logout 清除 cache；已经取得请求上下文的在途请求自行结束。

### `App::delete_profile`

`App::delete_profile(ProfileId)` 删除一个已保存连接及其凭据，语义固定：

1. 先取 `config_updates` 屏障并确认 App 仍打开，整个删除与同一屏障下的其他配置写入互斥。
2. 用 `App::profile` 查这个 id；**不存在的 id 直接返回 `Ok(())`**，所以重复删除是幂等的，不会报 `MissingProfile`。
3. 连接存在时调用已有的 provider `logout(&profile)`：API-key 连接清掉由 Endpoint 指纹决定的 `credentials.toml` 槽位，ChatGPT subscription 清掉 `providers/chatgpt-subscription/auth.json`，两者都丢弃进程内 volatile key。删除不另写一套凭据代码，凭据与连接身份的一致性规则与 logout 完全相同。
4. 再从 `$BONE_HOME/config.toml` 的 `profiles` 移除该 Profile。`DataStore::delete_profile` 委托给文件配置层：加锁、确认摘要未变、`retain` 掉目标项、重新校验并原子重写；id 不在文件里时既不改写也不创建文件，因此这一层同样幂等。
5. 随后收集所有非 releasing 的 Session handle，`drop` 句柄后调用 `reload_sessions(&targets, false)`，与 `apply_profile` 同形地让受影响的 Session 重新解析配置。
6. **悬空的 `ModelSelection` 不主动清理**：仍然选择该连接的 Session 保留原选择，`resolve_model` 产出 `ConfigProblem::MissingProfile`，由前端接管；TUI 因此在模型面板上显示 `Model setup needs attention`，用户重新选一个模型即可修复。App 不猜测替代连接，也不静默改写已保存的选择。

删除连接与从连接里移除一个模型是两件事：`resolve_model` 只要求 profile 存在，所以仅把某个模型从连接的 `models` 列表里移除（而不是删除 profile）不会让仍在使用它的 Session 变成 `MissingProfile`。

## Runtime 装配与旧会话背景

首次有 Queued 输入需要执行时，Session 惰性创建 Runtime。启动流程是：

1. 读取 User / Workspace / Session overrides 与 Profile；
2. 取得 API key，或为 ChatGPT 构造绑定私有 cache 路径的 Endpoint；
3. 构造 Worker / Coordinator `ModelAdapter`；
4. 以 canonical Workspace root 和 `ToolLimits` 构造工具；
5. 从 durable public history 中选取预算内的近期 `BootstrapContext`；
6. 创建 `Agent::with_ports_and_background`，持久化 RuntimeStarted 后投递输入。

App 不恢复旧 Runtime 的 future、Job ID 或工具调用。bootstrap history 是受限的只读材料；完整旧历史可由内置 `session_history` 工具按 `SessionSeq` 读取。该工具每次扫描一个耐久位置。如果单个公开事件超过 Runtime 的工具输出预算，它返回 `omitted: true` 并推进 cursor，不截断存储事实，也不让后续页永远不可达。

当前 bootstrap history 从 Session journal 起点线性扫描，再保留预算内的近期事件。它正确但随长会话增长；倒序索引或摘要属于后续性能工作。

## 写工具与外部效果

App 始终安装 `read`、`glob`、`grep`、`session_history`，以及受 App 包装的 `apply_patch`、`bash`。`ToolLimits` 只配置工具执行限制，不再保存工具模式。每个 Job 的工具授权由 Core 在创建时确定并保存，App 只把实际授权投影到 Job 详情。输入文本不额外携带权限配置。

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
- Session override 与项目配置的路径/摘要信任记录；
- Runtime config snapshot、原始 Agent Record 和公开 Session journal；
- 外部写入意图、结果和后续核查。

私有 storage 模块拥有 schema、document CAS、journal、事务、SQLite 连接和 OS lease。损坏、权限不安全或未知 schema 直接返回错误；App 不自动 reset、删除或猜测迁移。SQLite 用一条 App writer 连接串行短事务，WAL 允许并发读；独立连接使用有界 busy timeout。

这个版本不读取或迁移旧 SQLite 中的 User/Workspace settings 与 Profiles，也不读取、
导出或删除旧系统 Keyring 项；Session override 沿用原 SQLite schema。升级后用户需要重新
建立 Profile 并录入 API key，旧系统凭据由用户自行清理。

每个打开 Session 取得一个跨进程 writer lease，第二个进程打开同一 Session 会得到 `SessionBusy`。archive 只改变组织状态，不隐式取消或关闭 Runtime。

进程重启时：

- stale Runtime 在存储中关闭；
- 已经投递但未终态的 Input 标为 `Interrupted`；
- 能证明尚未投递的 Input 保持 Queued；
- Session metadata、draft、history、配置和 unresolved writes 恢复；
- 外部调用和 Job future 不恢复，未知写不自动重放。

`close_runtime` 只收尾 Agent 并保留 Session；稍后提交可以创建新 Runtime。`App::shutdown` 关闭所有 Session Runtime 和 provider 操作，清空进程内注册表，返回仍未解决写入。多次调用在完成后返回同一报告。

## 错误与前端契约

公开 `Error` 和 `AppProblem` 提供前端可匹配的边界，包括 Closed、Workspace / Session 不存在、SessionBusy、RequestConflict、StaleRuntime、WriteInProgress、InvalidState、Configuration、LoginRequired、Provider、Storage、Tools 和 Agent。

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
