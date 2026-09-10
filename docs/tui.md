# BONE TUI 产品与实现契约

状态：实现中。`bone-tui` 提供 `bone` 全屏终端前端。

本文是 TUI 当前唯一有效约定。设计稿用于解释视觉方向；若旧设计、handoff 或代码注释与本文冲突，以本文和 `bone-app` 的公开类型语义为准。

## 产品结构

工作台默认是左侧会话导航与中央工作区两栏。只有用户主动查看任务、上下文、产物、变更、证据或验收时，才展开右侧详情；中央工作区始终是视觉主体。

```text
AppShell
├── GlobalBar       工作区、需要你、设置
├── WorkspaceBody
│   ├── SessionRail
│   ├── MainSurface
│   │   ├── SessionWorkspace
│   │   ├── SessionBrowser
│   │   ├── AttentionView
│   │   └── SettingsView
│   └── DetailPane
├── ActionBar       当前焦点下可执行的可见操作
└── DialogLayer     创建、登录、选择与明确确认
```

代码按产品层级组织，而不是按页面堆成一个巨型文件：

```text
bone-tui/src
├── run/                 进程主循环、输入映射、App effect 执行、启动边界
├── state/               UI 所有权模型、唯一协议、唯一 reducer、状态测试
├── view/                页面编排
│   ├── components/      无状态可复用视觉与命中组件
│   ├── shell.rs         全局栏、会话导航、动作栏
│   ├── workbench.rs     会话正文与输入区
│   ├── attention.rs     需要你
│   ├── settings.rs      设置与登录
│   ├── details/         详情容器与单一绘制/命中分派
│   │   ├── work.rs      工作与未知写入判定
│   │   ├── changes.rs   工作区变更与分页正文
│   │   ├── context.rs   公共上下文
│   │   ├── artifacts.rs 产物、证据目录与来源阅读
│   │   ├── records.rs   持久事实记录
│   │   └── acceptance.rs 版本化用户验收
│   └── dialogs.rs       明确确认与编辑对话层
├── layout.rs            响应式区域与命中模型
└── terminal.rs          终端模式 RAII
```

组件是轻量的 Rust props + render 函数，不是新的 UI 框架。`GlobalBar`、`SessionRail`、`ActionBar`、`DetailTabs`、`DialogFrame` 与 `PagedReader` 只接收 `Rect`、只读 props、有限渲染缓存和主题；需要点击的组件从绘制使用的同一矩形导出 `HitRegion`。详情容器只有一个按 `DetailKind` 分派绘制与命中的入口，页面之间不复制第二套命中路由。组件不读取 App、不执行 I/O、不持有业务事实。页面负责把 `UiState` 投影为 props，reducer 仍是唯一状态转换入口。

布局规则不是建议值：

| 终端 | 布局 |
| --- | --- |
| `>=160` 列 | 左栏 24 列；右栏打开时 40 列；中央占余量 |
| `120–159` 列 | 默认左中；查看详情时隐藏左栏，显示中右 |
| `80–119` 列 | 单个主区域；会话与详情通过可见入口进入 |
| `40–79` 列 | 单区阅读，保留返回、输入和必要动作 |
| `<40` 列或 `<12` 行 | 窗口过小提示；不丢草稿与后台状态 |

设置与待处理事项替换中央主页面，不叠加会话输入框。返回会话时恢复该会话自己的草稿、阅读锚点和详情选择。

## 不可越过的 App 边界

TUI 只是前端。业务数据与产品操作全部经过 `bone-app`：

```text
keyboard / mouse / resize
           │
           ▼
      UI event ──► reducer ──► render
                         │
                         └──► typed effect ──► bone-app
                                                    │
                                  result / observe / history
                                                    │
                                                    └──► UI event
```

TUI 可以持有终端资源、焦点、选择、滚动位置、未提交编辑缓冲和有界渲染缓存。它不得查询 SQLite、读取项目文件、执行 Git、读取产物正文、访问 provider/凭据、依赖 Core/Adapter，也不得从自然语言或日志字符串推断完成、采纳、证据或验收状态。缺失能力必须先增加前端中立的 App API。

启动参数、精确 cwd、环境和终端设备属于进程启动边界。Workspace canonicalization、数据目录中的持久化内容和产品状态属于 App。

## 状态所有权与异步正确性

App 拥有 Workspace、Session、Input、Job、历史、配置、凭据、证据与用户判定。TUI 拥有页面、焦点、选择、每会话草稿和阅读位置。

每个异步请求携带 SessionId、操作身份和选择代次。迟到响应只有在身份和代次仍匹配时才能修改页面。后台事件可以增加未读或待处理计数，不能抢焦点，也不能把用户正在阅读的正文强行拉到末尾。

草稿提交遵循版本回执：

1. 使用稳定 `RequestId` 提交当前文本，并记录草稿版本；
2. pending 期间不重复提交同一请求，但允许编辑下一条内容；
3. 收到 durable `SubmissionReceipt` 后，只在草稿仍是原版本和原文本时清空；
4. 回执未知时以相同 `RequestId` 重试；
5. 保存或提交失败时保留内存文本并明确展示哪些事实已保存。

watch 只是可合并的最新快照，durable history 才是事件事实。前端保存 cursor，逐页补到 `history_through`；即使返回空页，只要 cursor 前进也必须保存并继续。历史按 sequence 去重，活动按稳定 Call 引用更新。

## 会话工作区

```text
SessionWorkspace
├── SessionHeader
├── ConversationView
│   ├── UserMessage
│   ├── AssistantMessage
│   ├── SystemNotice
│   ├── ActivityLine
│   └── NewContentMarker
└── Composer
    ├── ReplyContext
    ├── DraftEditor
    ├── SubmissionFeedback
    └── ComposerFooter
```

会话打开后从 App hydrate 快照与分页历史，再持续 observe。用户输入、问题、回复、工具结果、Job 结束和 Input 结束是不同事实：Reply 不代表请求完成，Job finished 也不代表用户输入已经完成。只有公开的 `InputFinished` 或后续正式验收记录能改变相应状态。

详情共享同一个容器和六个可见标签：工作、变更、上下文、产物、记录、验收。标题、分类、返回入口、来源行、正文阅读器和底部动作保持一致；不同详情只替换 typed 数据模型，不复制六套面板框架。

工作区差异由 App 以 Git HEAD 为基线生成，明确包含未跟踪文件，也可能含用户与其他任务的修改。任务证据使用持久稳定引用单独关联；TUI 不把当前 diff 猜成某个任务的产物。

文件列表和文件正文分别分页。正文窗口的 continuation 由 App 绑定 Git baseline、相对路径、来源和内容身份；文件或 HEAD 在阅读期间改变时，TUI 丢弃旧窗口并刷新列表，不把两个版本拼接。详情底部始终提供“上一窗口”“继续读取”和“返回文件列表”的可见控件，窗口替换而非无限累积正文。

产物页先锁定具体 `ResultRef`，再读取该结果明确引用的 `EvidenceRef`。来源目录区分公开、私有与缺失；私有来源连标题也不显示，工具参数永不进入来源正文。公开正文经 App 按最多 64 KiB 分页，TUI 只保留有界阅读窗口；离开缓存的正文通过 App continuation 重读，而不是使剩余内容永久不可达。结果、来源目录和正文请求分别携带 query、结果与来源身份，迟到响应不能替换当前结果或来源。

验收是用户针对特定结果版本作出的独立产品判定：接受、部分接受、带风险接受或退回，历史判定不可覆盖。退回会原子地保存判定与新的返工输入，绝不复活旧 Job。

## 输入与可发现操作

鼠标点击、滚轮和基础键盘导航是正式能力。所有关键动作在 GlobalBar、ActionBar、Composer footer、列表行或对话框中有可见入口；快捷键只是加速方式，不要求用户记忆命令表。

- `Tab` / `Shift-Tab`：移动区域焦点。
- 方向键：移动列表选择或滚动阅读区。
- `Enter`：发送或确认当前明确动作。
- `Shift-Enter` / `Alt-Enter`：输入换行（终端能力不一致时使用可见“换行”动作）。
- `Esc`：只关闭当前详情/对话层，或返回会话；绝不隐式停止工作。
- `Ctrl-N`：新建会话的辅助快捷键。
- `Ctrl-Q`：发起安全退出；有风险时显示确认。

粘贴永远只插入文本，不执行命令或本地动作。普通文本与本地动作由事件类型明确分隔。停止、发送、换行、更多操作和返回都有可点击/可聚焦入口。

## 设置、登录、待处理与恢复

设置页同时展示 App 返回的 saved desired 与 currently applied 状态；保存成功但应用失败时二者分开展示，不能声称已经生效。配置 scope、继承来源和修复动作由 typed App 数据表达。

登录由 App 的登录 attempt 和 typed state 驱动。订阅连接使用 App 的登录 attempt；API 连接通过遮罩输入框把不可调试输出的 secret value 交给 App 凭据接口，TUI 不读取或回填已保存值。API key、OAuth payload、authorization header、完整 prompt/文件正文与 device code 不进入历史、诊断或模型上下文。需要你页面汇总待回答问题、未知写入、配置应用失败、登录动作和可恢复中断；摘要查询不得获取所有 Session 的 writer lease。

未知写入和中断不会自动重放。用户核对 App 提供的稳定 Session/Call/结果证据后才能解除写门禁。强制退出或旧 Runtime 丢失后，用户通过一条新的明确要求恢复工作。

## 终端生命周期与性能

raw mode、alternate screen、鼠标捕获、bracketed paste 和光标由一个 RAII guard 管理，覆盖正常退出、初始化错误和 panic。正常退出给草稿一次严格有界的 flush；第二次终止信号会停止等待。无论草稿或 App 收尾是否超时，都先离开全屏终端并恢复 shell，再报告错误；`App::shutdown` 自身也有上限。

渲染是无 I/O 的纯函数；布局 Rect 同时用于绘制和鼠标命中。事件驱动并仅在 dirty 时绘制，连续更新合并到最多 30 FPS。正文只排版可见窗口并按内容版本/宽度缓存。历史与正文缓存按字节受限，默认总预算 32 MiB；草稿不属于可淘汰缓存。

外部文字进入渲染树前去除 ANSI、OSC 与其他不可见控制序列，保留必要的换行与制表。Unicode 切分、宽度、光标位置和鼠标命中以显示宽度而非字节数计算。

性能、故障注入、PTY、跨平台 CI 与人工终端验收的固定门禁见 `docs/tui-quality-gates.md`。
