# TUI：下一阶段前端设计

状态：计划中。当前 workspace 没有 TUI crate 或 `bone` binary。

第一版 TUI 是 `bone-app` 的终端前端。它负责终端生命周期、键盘、布局、焦点、命令面板和展示状态；所有 Workspace、Session、配置、登录、持久历史和 Agent 控制都通过公开 App API 完成。

```text
terminal events ─┐
App watch ───────┼──► TUI reducer ──► render model ──► Ratatui
effect results ──┘         │
                           └──► typed effects ──► bone-app
```

TUI 不直接依赖 `bone-core`、`bone-adapters` 或 SQLite。若一个交互无法由 `App` / `Session` 表达，应先扩展 headless App API 和测试，再接界面。

## 产品边界

启动 `bone` 时，当前工作目录是本次前端选择的 Workspace root：

1. 前端解析平台用户数据目录并调用 `App::open(AppOptions)`；
2. 把精确的启动 cwd 传给 `open_workspace`；
3. 列出该 Workspace 的 Session，打开选中的一项或创建新 Session；
4. 订阅 `Session::observe` 并分页恢复历史；
5. 立即显示工作台；网络连接和 Agent Runtime 只在输入需要执行时发生。

Workspace canonicalization 和稳定 ID 由 App 完成。TUI 不寻找 Git root、不改变运行中的 Workspace，也不在项目内创建 `.bone/`。Git branch 等信息以后可以作为只读装饰，不参与身份。

一份 Session 是 durable 产品对象，一份 Runtime 是按需创建的进程内执行对象。打开历史、编辑草稿或修改配置不要求 Runtime 已存在。退出前端不会删除 Session；正常 `App::shutdown` 会持久化 Runtime 收尾事实，进程异常退出后再次打开则把已经投递但失去 Runtime 的输入标为 `Interrupted`。

第一版只做交互全屏入口。one-shot CLI、HTTP server 和桌面壳可以以后复用相同 App API，但不进入 TUI reducer。

## 模块与所有权

建议新增独立 `bone-tui` crate，并由它提供 `bone` binary。内部模块保持少而清晰：

```text
bone-tui/src/
├── main.rs        参数、data dir、cwd 与进程退出码
├── terminal.rs    raw mode、alternate screen、paste、panic/Drop 恢复
├── run.rs         event loop、异步 effect 与 Session observer fan-in
├── state.rs       AppEvent、reducer、focus、overlay 与 presentation state
├── commands.rs    typed command registry、解析、可用性与补全
└── view.rs        纯 &State → Frame 渲染
```

只有 reducer 修改 presentation state。终端输入、App watch、历史分页、登录和配置调用都产生 `AppEvent`；异步 effect 完成后把结果再送回 event loop。render 函数不做 I/O、不拿锁、不调用 App。

资源归属：

| 资源 | Owner |
| --- | --- |
| terminal guard 与 event stream | TUI runner |
| `bone_app::App`、打开的 `Session` handles | TUI runner / effect layer |
| Session observer tasks | effect layer，以 SessionId 标记 |
| timeline、composer、focus、scroll、overlay stack | reducer state |
| Runtime、provider、credential lease、SQLite | bone-app |
| Job、Call、Record 与工具执行 | bone-core / bone-adapters，经 App 投影 |

observer 只能发送带 `SessionId` 的事件，不能直接修改选中页或绘制终端。Session 切换只改变 selection；后台 Session 的任务、observer 和草稿保持各自状态。

## 展示状态

前端状态分成三个正交部分：

- 产品投影：每个 Session 的最新 `SessionView`、已加载 timeline 和 history cursor；
- 本地交互：当前 Session、composer 编辑缓冲、滚动锚点、unread 和当前焦点；
- surface：Workbench / Sessions，以及 Command、Model、Settings、Login、Diagnostics、Confirmation、Recovery 等 overlay stack。

不要把 App 的 `RuntimeState`、Input / Job 状态和 TUI surface 合并成一个“大状态枚举”。一个 Session 可以同时是 Running、当前未选中、有 unread，并且用户正在另一个 Session 的 Settings overlay 中。

Overlay 采用栈。栈顶先收到普通按键，关闭时恢复打开前的 focus token；后台事件可以使原控件失效，但不能抢焦点。文本输入聚焦时，单字母快捷键永远是文本。

## Workbench

宽屏建议使用 Session rail、timeline、composer 和一行 status：

```text
 BONE                         │  user input
                              │
 Sessions                     │  assistant reply
 ▌ Fix CI        • Working    │
   Auth          ? Waiting    │  ✓ read …
   Parser        ✓ Complete   │  • tool activity …
                              │
 Ctrl-N New                   │  ┌ composer ─────────────────┐
 /sessions                    │  └───────────────────────────┘
 /workspace/path              │  model · state · shortcuts
```

窄屏隐藏 rail，在 header 提供当前序号、Session 总数和最高优先级 attention；`/sessions` 或可聚焦 header 打开全屏 Session list。宽窄布局必须共享同一 selection、timeline、composer 和命令行为。

timeline 由 durable `SessionEvent` 构成：

- 用户输入、问题、回复、Job completion 和 Input completion 是独立行；
- ToolFinished 显示工具名、成功 / 失败和外部效果；大正文默认折叠；
- RuntimeStarted / Reconfigured / Closed、Interrupted 和 WriteResolved 是可解释的系统边界；
- 当前 `SessionView::activity` 是 timeline 尾部的可变 activity，不伪装成 durable 历史。

`Reply` 不等于 Input 已完成，JobFinished 也不等于整条用户请求已完成。界面只有看到自己的 `InputFinished` 才标记该输入结束。

颜色不能成为唯一状态。每个 badge 同时使用短文字：

```text
Ready · Starting · Working · Waiting for you · Stopping
Complete · Failed · Interrupted · Needs setup · Unresolved write
```

## 无丢失观察

每个 Session 使用 App 规定的 watch + history 协议：

1. 取得 `let receiver = session.observe()`；
2. 读取当前 `SessionView` 与 `history_through`；
3. 从本地 cursor 调用 `history`，直到补到该水位；
4. 收到 watch changed 后替换 View，再继续分页；
5. history 返回空 items 但 cursor 前进时仍保存新 cursor，并按 `has_more` 继续。

watch 允许合并通知，所以不能用“每次 changed 对应一个 timeline item”的假设。observer 重启也从最后 `SessionSeq` 补读。timeline 以 sequence 去重，当前 activity 以 `CallRef` 更新。

第一版 App 只提供完整模型回复和 Call progress，没有 provider token delta 的前端事件。因此 TUI 显示结构化 activity 和完成后的 Reply。需要 token streaming 时，应先在 Core / App 定义可丢失的展示 delta 与唯一 durable terminal 之间的关系，再扩展 API；前端不能直接订阅 ModelAdapter。

## Composer 与提交

Composer 始终存在，即使没有模型、尚未登录或 Session 暂时不能执行。编辑缓冲属于 UI，TUI 通过 `Session::save_draft` debounce 保存；切换 Session、打开阻塞 overlay 和正常退出前立即 flush。save 失败时保留内存文本并显示持久化问题。

发送顺序：

1. 保留 composer 文本，发起 `Session::submit`；
2. 提交 pending 期间禁止重复发送同一个 `RequestId`，但允许继续编辑下一条 draft；
3. 只有取得 `SubmissionReceipt` 后才从 composer 清除对应文本并显示 durable input；
4. 如果调用方丢失回执，用相同 RequestId 和内容重试；
5. Storage / validation 失败时原文留在 composer；配置或登录问题发生在 durable submit 之后时，输入显示 Queued 而不是回到未保存草稿。

收到 `WaitingForUser` 时，composer 标明问题上下文。发送回答必须用 `SubmitInput::answer(question_id)`；如果回答随后因 stale question 进入 `Rejected`，恢复并保留回答文本，让用户选择作为普通新输入发送。

运行中继续输入是正常路径，不要求先 Stop。Esc 在没有 overlay 时发出当前 Session stop intent；有 overlay 时只关闭栈顶，绝不能同时停止 Agent。

## Session 导航

Session rail / picker 默认只列出当前 Workspace 的 Session。archive 默认从主列表隐藏，但保留在可筛选列表中。创建、重命名和 archive 分别调用 App API，不根据 UI selection 猜 durable ID。

选择 Session 时：

- 已经打开的 handle 和 observer 直接复用；
- 尚未打开的 Session 调用 `App::session`，然后按 history 协议 hydrate；
- 不创建第二个 Runtime，也不重新发送历史输入；
- 每个 Session 保留自己的 composer、cursor 和 scroll anchor。

当前 App 在打开 Session 时取得跨进程 writer lease。若 lease 被另一进程持有，`App::session` 返回 `SessionBusy`；第一版 TUI 应把该项标为 Busy，并提供刷新 / 返回，不能忽略锁后双写。App 暂无不取得 lease 的 read-only Session API，因此“只读打开 Busy Session”是明确的后续 App 能力，前端不能直接读 SQLite 实现。

Interrupted 表示旧 Runtime 的 future 和网络 / 进程工作已经丢失。界面保留历史，并允许用户写一条新的明确请求；不要把旧 Input 自动重放。存在 unresolved write 时，优先展示工具、Call 和已知 outcome，让用户核查后调用 resolve_write。

## Typed slash commands

slash command 由一个 registry 定义 name、参数、作用域、可用条件、effect 和说明。它们在本地调用 App API，不进入模型上下文。第一版建议提供：

```text
/help                 帮助与键位
/status               当前 Workspace、Session、Runtime、model 和问题
/workspace            canonical Workspace 信息
/new                  创建 Session
/sessions             打开 Session picker
/rename               重命名当前 Session
/archive              archive 当前 Session
/model                修改 Worker model，明确 Session/Workspace/User scope
/coordinator          修改 Coordinator override
/tools                修改 ToolMode / limits
/login  /logout       显式 provider credential flow
/stop                 停止当前 Session 的当前工作
/close                关闭当前 Runtime，保留 Session
/quit                 安全退出
```

命令解析不是 shell：

- 只解析单逻辑行中第一处非空字符为 `/` 的 typed 输入；
- unknown command 留在 composer 并显示建议，不能静默发送给模型；
- `//text` 发送普通消息 `/text`；
- paste event 只插入文本，永不触发执行或 Enter；
- 多行 paste 始终是普通 prompt；
- 单行粘贴的 slash text 默认按普通消息处理，只有用户明确选择才作为 command；
- 不展开 `$VAR`、`~`、glob、管道、重定向或 command substitution。

复杂设置通过 overlay / picker 选择，不把 raw provider JSON 暴露为命令参数。

## 模型、设置与登录

界面必须同时展示 resolved desired 与当前 running config，来源是 `App::resolved_config`：

```text
Session override > Workspace override > User setting
```

每个设置操作明确 scope；“inherit”通过发送 `ConfigChange::<field>(None)` 清除当前 override。Coordinator 未单独设置时跟随 Worker，这一继承关系要直接显示。

调用 `update_config` 后，UI 进入 Applying，并等待 Future 与 Session watch：

- 成功表示所有受影响的本进程 Session 已处理配置；
- Running Runtime 保留身份和 Job 图，旧模型 Call 被撤销，新调用使用新配置；
- 已经开始的 Tool 仍按旧 port / limit 收尾；
- 装配或持久化失败时 saved desired 与 running 可能不同，Session 暂停新调度并给出问题；
- fan-out 部分失败时逐个 Session 展示自己的 desired / running，不能声称全局 rollback。

不要复用旧设计中的“所有 running Runtime 永远 pinned 到关闭”为当前契约。也不要发明 Last Known Good / configuration revision UI；App 公开的是 desired、running 和 typed problem。

Profile 和 credential 分开：

- Profile 保存 protocol、HTTPS endpoint 和 display label；
- `ModelSelection` 保存 profile、model 与 typed `ModelOptions`；
- API key 通过系统 credential manager 设置，不显示或回填完整值；
- ChatGPT 登录由 `App::login` 返回的 `LoginAttempt::observe` 驱动 DeviceCode overlay；
- 普通 Runtime 遇到 `LoginRequired` 只显示恢复动作，不自动打开 OAuth；
- logout 返回 ProfileBusy 时保持连接与 cache，不伪装成已经退出。

device code 只在当前登录 overlay 生命周期内显示，不进入 timeline、诊断导出或 model context。

## 错误、恢复与诊断

错误展示回答三个问题：发生了什么、哪些数据已经保存、用户现在可以做什么。默认文案匹配 `Error` / `AppProblem` enum；技术字符串放进可展开详情，不用字符串解析决定按钮。

| 问题 | 主界面行为 |
| --- | --- |
| NeedsModel / MissingProfile | 历史与草稿可用；打开 model / profile 设置 |
| LoginRequired | 保留 queued input；提供显式 login |
| ProfileBusy | 说明另一个连接 / login / logout 占用；允许重试 |
| SessionBusy | 不打开可写 handle；刷新或返回 |
| Storage | 停止接受会造成假 durable 确认的操作；保留 composer 内存内容 |
| Provider / Agent / Tools | 保留已保存输入和 running / queued 事实；提供 reload / stop / close |
| Interrupted | 显示旧 Runtime 已丢失；不自动重放 |
| unresolved write | persistent warning；禁止新的 Workspace write，进入核查流程 |

存储 corruption、schema mismatch 或权限错误不能触发自动 reset / 删除。诊断可以显示 data dir、Workspace、Session / Runtime IDs、provider kind 和 redacted error chain，但不得包含 API key、OAuth payload、authorization header、完整 prompt / file 内容或 device code。

## 退出与终端恢复

Ctrl-C 或 `/quit` 发出退出 intent。有 active work、pending submit、未保存 draft 或 unresolved write 时，confirmation overlay 展示影响；确认后停止接受新 UI action。

退出顺序：

1. flush 当前可保存 draft；
2. 结束 terminal input task；
3. 立即离开 raw mode、alternate screen，恢复 bracketed paste、cursor 和 panic hook；
4. 在普通终端画面调用 `App::shutdown` 并显示必要的短状态；
5. 若报告 unresolved writes，打印可再次打开核查的 Session / Call 标识，返回非成功或显式警告状态。

终端恢复必须由 RAII guard 保证，覆盖正常退出、error、panic 和 task cancellation。慢 provider / Agent shutdown 不应让用户长时间困在 alternate screen。

## 键盘基线

具体绑定集中定义并可配置，第一版至少保证：

| 位置 | 按键 | 行为 |
| --- | --- | --- |
| 全局 | Ctrl-C | 退出 intent |
| 全局 | Ctrl-N | 新 Session |
| Workbench | Ctrl-Left / Ctrl-Right | rail 与 composer 之间移动；终端不支持时用命令入口 |
| Composer | Enter | 发送或确认当前 command |
| Composer | Ctrl-J | 插入换行 |
| Timeline | PageUp / PageDown | 翻页 |
| Timeline | Ctrl-Home / Ctrl-End | 最旧位置 / live tail |
| 无 overlay | Esc | stop intent |
| Overlay | Esc | 关闭一层并恢复焦点 |
| 列表 | Up / Down、Enter | 移动与选择 |

必须在 40、80、120 列、CJK 双宽字符、`NO_COLOR` 和常见终端上验证。窗口过小时显示提示并保留 draft、后台工作和退出能力。

## 第一版发布范围

第一版完成条件：

- 从任意已有目录进入工作台，不等待网络得到首帧；
- 无模型 / 未登录时仍能查看 Session、编辑并保存 draft、完成设置；
- submit 只在 durable receipt 后清 composer，RequestId 重试不会重复输入；
- watch + history 在合并通知、空公开页、切换和重开后不丢 timeline；
- 多 Session 独立保持草稿、滚动、observer 和后台状态；
- 配置界面准确展示 scope、desired、running 和 applying / failure；
- WaitingForUser、stop、close、Interrupted 和 unresolved write 都有可操作流程；
- slash paste 不执行本地命令，local command 不进入模型；
- SessionBusy 不产生第二个 writer；
- storage / provider / Agent 错误不丢 durable 数据或伪造完成；
- terminal 在所有退出路径恢复。

首版不要求 token streaming、model catalog、跨进程配置实时通知、Busy Session 只读打开、安全接管、Session 删除、自动 worktree、云同步或进程退出后的后台执行。这些能力需要相应 App 契约和测试后再加入。

## 测试边界

TUI 测试分三层：

- reducer table tests：焦点、overlay、Session 切换、command availability 和异步结果乱序；
- layout / snapshot：宽窄 terminal、Unicode width、长文本、问题和错误状态；
- 少量 App integration：真实临时 SQLite 与脚本化 provider，完成首次设置、durable submit、history catch-up、配置切换、stop、恢复和 shutdown。

不要在 TUI 测试 Core Job 规则或 provider wire JSON。完整策略和 TUI 前发布门禁见 [Testing](testing.md)；可用 API 与恢复语义见 [App](app.md)。
