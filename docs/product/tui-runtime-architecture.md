# BONE TUI 产品运行时架构

状态：第一阶段 durable Workspace/Session 已落地；第二阶段已完成 reducer/effect
边界收口。这是后续实现的边界合同，而不是把所有业务塞进 `App` 的许可。

## 结论：产品 TUI 已具备严格的 presentation UDF 边界

产品路径现在遵循：

```text
AppEvent → App::reduce() → Action（effect request）
                              ↓
                 product effect modules
           (session controller / commands / runtime)
                              ↓
                    AppEvent::Succeeded / Failed
                              ↓
                         App::reduce() → pure view
```

这里的“严格”指 presentation state：产品 runner 不再直接调用 `app.set_*`、
`app.attach`、`app.clear_composer` 等 mutation method。草稿写入、durable turn
接受/拒绝、runtime attach/receipt/failure、连接状态、session hydrate、模型就绪状态、
notice 和 layout measurement 都作为 `AppEvent` 回灌 reducer。一个
`tokio::select!` UI loop 是 `App` 与 terminal 的唯一并发写者；observer 只发送带
Session ID 的更新；渲染保持 `&App → Ratatui frame` 的纯投影。

这不表示所有 effect 已经异步化：当前一部分 SessionStore、journal 和 SettingsService
调用仍在该 UI loop 内同步执行。它们已经不再能直接改变 UI，但慢盘、锁竞争或网络文件
系统仍可能影响交互。下一阶段应把 `Action` 进一步显式化为 `Effect`，并把阻塞存储迁到
storage worker / `spawn_blocking`，再将结果送回同一个 event queue。

因此当前状态准确地说是：**严格的 UI 状态单向流 + 仍待异步化的 effect executor**。

## 当前产品路径

```text
启动目录
  │
  ├─ WorkspaceApplication
  │    ├─ WorkspaceRegistry：canonical launch directory → durable Workspace ID
  │    └─ SessionStore：SessionRecord、draft、journal
  │
  ├─ SettingsService（可失败；失败只进入 repair state）
  │    └─ ConfigManager：typed JSON、CAS、atomic persistence
  │
  └─ TUI shell
       Terminal / runtime update
               │
               ▼
       AppEvent → App::reduce() → Action
                                      │
                                      ▼
                              workspace runner
                              ├─ session controller: store / journal / lease
                              ├─ command effects: SettingsService
                              ├─ runtime driver: AgentHost / attach
                              └─ login connection
                                      │
                                      ▼
                         typed success / failure AppEvent
                                      │
                                      ▼
                              pure Ratatui rendering
```

`bone-agent` 的 Kernel/Runtime 不被 TUI 重写：Kernel 保持纯状态机，Runtime
继续负责副作用与 session actor。产品层只协调 logical Session 与可选的
RuntimeAttachment。

## 领域对象与所有权

| 对象 | 权威位置 | 生命周期 | 不应包含 |
| --- | --- | --- | --- |
| Workspace | `bone_app::WorkspaceContext` | 用户从目录启动后稳定 | Git 根猜测、Agent handle |
| SessionRecord | 私有 SessionStore | 跨重启 | credential、runtime future |
| SessionJournal | 私有 JSONL | 跨重启、只追加 | 自动重放的 tool request |
| RuntimeAttachment | `runtime_driver`（进程资源）与 `session_controller`（durable summary） | 当前进程 | durable identity |
| App | TUI loop | 当前终端 | 配置文件、credential、长期业务 policy |
| Projection | `Conversation` | 当前显示 | Agent 决策逻辑 |

一个用户目录就是一个 Workspace；不会写 `.bone/` 到项目。一个 Workspace 可以有多个
durable Session；它们在界面中可见，即使没有登录、没有模型或没有 attached runtime。

## 可靠发送边界

新用户消息的当前写入顺序是：

```text
draft
  → resolve effective model/config
  → append + fsync UserTurnAccepted { turn, text, revision, model }
  → UI 显示用户消息并清空 composer
  → attach runtime / AgentHandle::post
```

`UserTurnAccepted` 是一个原子 journal fact：文本、turn ID、实际选择的 model 和
config revision 不再拆成两个 append。只要它写入失败，composer 保持不变，runtime
绝不会收到消息。

Runtime 的普通 `Step` 与 broadcast lag 后的 `Reset` 都走同一套“按本 runtime
cursor 去重并持久化”的路径，避免 Reset 显示了补回的回答、重启后却丢失。

当前 Agent notice 没有可用于并发持久 turn 的关联 ID。因此一个 logical Session 暂时只
接受一个未终结的 durable turn；用户可以继续编辑草稿，但要等现有 turn 终结或停止后
再提交。以后若要支持同一会话的并发/插队 message，必须把 durable turn ID 或
`MessageReceipt.id` 的关联下沉到 Agent 事件协议，而不能由 TUI 猜测。

已接受但 runtime 尚未接收的消息保持 pending；连接或启动失败时状态从 `Opening`
转为清晰的 detached/retry 状态，而不是卡住或假装已执行。用户通过 `/login` 显式重试
连接。意外关闭与进程退出会留下 `RuntimeInterrupted`；未确认外部写会留下
`UnresolvedExternalEffect`，恢复时绝不自动重放。

这里的“重试”只适用于**当前进程中、尚未收到 `AgentHandle::post` receipt 的 pending
effect**。冷启动时，`post` receipt 与 `TurnStarted` journal append 之间可能已经发生
崩溃；没有 receipt fact 不能证明 runtime 没有收到消息。为了避免重复模型调用或工具写入，
新进程会将任何未终结 turn 写成明确的 interruption；缺少 `TurnStarted` 证明的 turn 额外标
`RecoveryNeeded`，而不会自动 `/login` 重投。未来若要支持安全自动恢复，必须先将 durable
turn ID 下沉进 Agent/Kernel 并提供幂等 receipt 协议。

`SessionRecord.status` 现在是**重启时可解释的最后已知摘要**，不是第二份历史：

| 已确认边界 | durable summary |
| --- | --- |
| `UserTurnAccepted`，尚未收到 runtime receipt | `QueuedForRuntime`；attached runtime 仍不等于已执行 |
| 已排队 start | `Opening + Attaching` |
| runtime observed | `Opening + Attached` |
| `AgentHandle::post` 返回 receipt | `Working + Attached` |
| Paused / Stopped / Finished | `WaitingForUser` / `Ready` / `Complete` |
| start、connection 或 receipt 失败 | `QueuedForRuntime + Detached`，仍是本地可编辑状态 |
| active runtime 关闭 | `Interrupted + Detached`；idle terminal runtime 只改为 detached |
| cold start 遇到未终结 journal turn | 先写 `RuntimeInterrupted + Detached`；没有 durable receipt proof 时加 `RecoveryNeeded` |

这些 summary 都经 `SessionStore::replace` 的 CAS 保存；遇到另一进程刚写入同一
record 的普通 revision conflict，runner 会 reload 一次并只重放本次负责的字段，避免
缓存 revision 永久陈旧。journal append 和 record CAS 仍是两个独立的文件边界，所以
journal 始终优先：summary 写失败会展示可见错误而不回滚、也不会重复 append 历史事实；
下次冷启动会从 terminal/interruption journal facts 单向修复 summary。

现在每个 logical Session 都有一个独立的 **writer lease**：它是 Session sidecar 文件上
的长期排他 OS file lock（由 `fs2` 映射到 Unix/Windows 原生锁），不是会因时钟偏差而失效
的 TTL。产品启动会先取得选中 Session 的 lease，**再**更新 `last_opened_at`；因此第二个
BONE 进程不会在失败后仍改写该 Session 的 record。lease 在正常退出时 drop，在进程崩溃时
由 OS 自动释放。

TUI 不会为了显示列表而占住整个 Workspace：非选中 Session 只读 hydrate，不做 cold
recovery、模型 readiness 或 summary 写入。用户切换或 `/resume` 到一个 Session 时才尝试
取得 lease：成功后重新读取 journal、执行同一套 cold recovery，再允许编辑；失败则在本
进程呈现 `ReadOnlyElsewhere`，并由 reducer 阻断草稿、命令、发送和 stop Action。成功切换
后，旧 Session 若没有 live runtime、pending/start task 或 active durable turn，会释放其
idle lease；仍在执行/收尾的 Session 保留到完成或 shutdown。

这消除了两个 BONE 进程为同一个 logical Session 同时分配 turn ID、append journal 或
attach runtime 的产品路径。它不改变短生命周期的 record CAS/journal lock：它们仍分别用来
线性化一次文件操作、处理损坏/陈旧写入。网络文件系统若不可靠地实现原生文件锁，无法从
本地进程协议获得同等级保证，属于部署环境限制。

## 配置与模型语义

`SettingsService` 是唯一的交互设置入口。模型优先级为：

```text
Session override > Workspace default > User default > agent.system default
```

`/model` 的写入立即持久化。`/model default` 和 `/model global` 会立即刷新当前进程中
已持有 writer lease 的 Session；背景只读 Session 会在本进程后来取得 lease 时重新解析
模型并刷新 readiness。这样既不会把另一个 BONE 进程拥有的 record 改写为本地状态，也不会
让旧的 `NeedsSetup` 缓存成为后续编辑的阻塞原因。

但现有 `AgentHost::start()` 在创建 runtime 时构造固定的 `ModelAdapter`。因此当前真实
契约是：

```text
设置已保存立即可见
已 attached runtime 保持其 pinned model
新建 / 重建的 runtime 使用新 model
```

不能把它描述为“live runtime 的下一条消息已切换模型”。要兑现这个更强的产品承诺，
必须新增 runtime 确认的 per-turn 配置 API，例如：

```text
AgentHandle::post(text, TurnConfig)
  → runtime 在安全 turn boundary 安装 config
  → ConfigApplied acknowledgement
  → AppEvent::ConfigApplied
```

在此之前，UI 必须呈现 `saved / pending next runtime`，而不是虚假的 `applied`。

## 下一步：把 effect executor 移出 UI loop

现在已落地的 result events 包括：

```rust
DraftPersisted | DraftPersistenceFailed
TurnAccepted | TurnRejected
RuntimeStartQueued | RuntimeAttached | PendingPostAcknowledged | RuntimeStartFailed
ConnectionStarting | ConnectionSucceeded | ConnectionFailed
SessionHydrated | SessionModelReadiness | SessionNeedsSetup
SessionReadOnlyElsewhere | SessionWriterLeaseAcquired
Notice | ComposerCleared | ViewportMeasured
```

接下来把 `Action` 改为独立的 typed `Effect`，使 executor 只能执行 I/O 或网络；所有
同步存储 work 通过 storage worker / `spawn_blocking` 返回同一个 AppEvent channel。这样
可以保留当前 UDF 不变式，同时消除慢盘卡住 redraw 的风险。

下一项恢复一致性工作是让 Agent/Kernel 认可 durable turn ID 并返回幂等 receipt。writer
lease 已保证同一 Session 同时只有一个本地写入/运行者；新的进程仍会将上次写入的
`Attached` runtime 归一化为明确的 detached/interruption 边界，而不是尝试恢复内存
runtime。journal 继续负责历史事实与恢复证据，status 只负责启动时的快速、真实摘要。

## 不变式

- 只有一个 TUI loop 写 `App` 和 terminal；observer 只发送带 Session ID 的事件。
- 渲染不做 I/O、不启动 Agent、不修改状态。
- 用户消息在清空 composer 前必须已 durable；journal 异常时不执行。
- SessionID、WorkspaceID、draft、journal 永远不依赖当前进程的临时 UI ID。
- Session 不因未登录、缺模型或损坏的设置而从界面消失。
- 对未知外部副作用和中断只记录、展示、要求用户决定；绝不自动重放。
- TUI 不声称已经实现 runtime 没有确认过的配置切换。

相关产品层需求与交互设计见：

- [PRD](tui-workspace-prd.md)
- [交互设计](tui-interaction-design.md)
- [静态设计稿](bone-tui-design.html)
