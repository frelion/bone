# BONE TUI 前端交互设计稿

| 字段 | 内容 |
| --- | --- |
| 文档状态 | Ready for product & engineering review |
| 对应 PRD | [TUI 配置、Workspace 与多 Session PRD](tui-workspace-prd.md) |
| 目标终端 | 40、80、120 列代表性布局 |
| 主要输入 | 键盘；鼠标为未来增强，不作为主流程依赖 |
| 最后更新 | 2026-09-07 |

## 1. 设计结论

BONE 的主界面始终是当前工作目录的多 Session 工作台。配置、认证、模型选择和错误修复都是工作台中的界面状态，不是进入工作台之前的门槛。

面向用户统一使用：

- 工作目录；
- 对话；
- 设置；
- 模型；
- 当前任务；
- 所有项目。

普通界面避免使用：

- `workspace_id`；
- `SessionRuntime`；
- `agent.system`；
- `ConfigSection`；
- `ConfigRevision`；
- `credential_root`；
- `Provider error`。

这些术语只允许出现在用户主动打开的技术详情中。

## 2. 体验原则

1. **首帧优先**：TUI 首帧不等待配置、认证、模型目录或网络。
2. **配置不可见**：`/config` 是设置中心，不是 JSON 查看器。
3. **一次选择就是一次提交**：没有总保存按钮，没有“保存并重启”；提交仍须经过校验、准备、持久化和运行时确认。
4. **状态真实**：只有 Effective revision 被对应运行时确认后才显示“已应用”，文件写入成功不等于运行时生效。
5. **安全边界明确**：一个 User Turn 锁定一份 TurnConfig；进行中的 Turn 不换模型或 Agent 行为，显示“下一条消息生效”。权限收紧是例外，可立即阻止尚未开始的危险操作。
6. **范围默认保守**：模型选择默认只影响当前对话；设置中心默认修改当前工作目录。
7. **后台不抢焦点**：Session 完成、模型验证完成、配置刷新都只提示，不切换当前页面。
8. **关闭 Overlay 不停止任务**：`Esc` 永远先关闭最上层界面；在 Sessions surface 时返回 Composer；只有栈为空且 Workbench/Composer 聚焦时才是当前 Session 的停止键。
9. **失败保留成果**：草稿、历史和旧的有效设置永远优先保留。
10. **颜色不是唯一信息**：所有状态同时使用符号和文字。

## 3. 信息架构

```text
BONE App Shell
├── Workspace Context
│   ├── 固定工作目录
│   ├── 账号与连接
│   ├── 有效配置
│   └── Session Registry
├── Session Navigation
│   ├── 当前工作目录的 Session
│   ├── 状态与未读
│   └── 新建、恢复、重命名、归档
├── Workbench
│   ├── Conversation timeline
│   ├── Live activity
│   ├── Composer
│   └── Contextual status line
├── Overlay
│   ├── Command Palette
│   ├── Settings Center
│   ├── Model Picker
│   ├── Session Picker
│   ├── Authentication
│   ├── Diagnostics
│   └── Confirmation / Recovery
└── Non-blocking Feedback
    ├── Toast
    ├── Inline field state
    ├── Local timeline event
    └── Persistent warning banner
```

阻塞式界面使用 **Overlay stack**，而不是一个可被覆盖的 `Option<Overlay>`。常见栈例如 `Settings → Model Picker → Confirm`；只有栈顶接收输入，关闭后恢复到其父 Overlay 中原来的字段。Toast、后台 Session 状态、rail 标记和持续警告可与栈并存，但不能入栈或接管焦点。

## 4. 前端状态模型

应用 Shell、逻辑 Session、进程内 Runtime 和界面层必须分开建模。Setup、网络或模型不可用不会销毁 Shell，也不会阻止读取本地 SessionRecord。

### 4.1 应用生命周期

```text
Shell phase:       Bootstrapping | Interactive | TerminalUnavailable
Setup state:       Unconfigured | Authenticating | LoadingModels | Ready
Connection health: Disconnected | Connecting | Connected | Degraded
Storage health:    Initializing | Writable | ReadOnly | RepairNeeded | Unavailable
```

这些轴可以组合。例如 `Interactive + Unconfigured + Writable` 是正常的未配置工作台；`Interactive + Ready + Degraded` 仍允许查看历史、写草稿和修复连接。`Storage.Unavailable` 是 TUI 内的阻断页：它阻止接受新消息，但不能在首帧前把用户丢回 Shell。只有终端本身无法初始化时才允许首帧前退出。

### 4.2 逻辑 Session 与惰性 Runtime

`SessionRecord` 是用户看到并可恢复的对话；`AgentRuntime` 是完成工作时临时附着的进程内执行对象。两者不是同一个生命周期。

```text
SessionRecord（可持久化）               AgentRuntime（不跨进程）
├── session_id / workspace_id           ├── None / Attaching / Attached
├── 标题、历史、草稿、滚动位置           ├── 当前 AgentHandle 与任务
├── Session 配置覆盖与 revision          ├── Runtime lease
├── 中断与风险恢复数据                   └── 可按需释放并再次创建
└── 不依赖账号、模型或连接即可存在
```

启动 Workspace 时只加载 Session 索引、上次选中项及其必要历史。查看历史、切换 Session、编辑草稿、重命名和归档都不创建 Runtime。只有用户提交消息或明确发起新的恢复工作时才异步附着 Runtime；当前进程中已经在后台工作的 Session 只复用既有 Runtime。同一个 Session 在同一时刻最多有一个可写 Runtime lease。

Session 状态必须使用正交维度，不能把 `Archived`、`Working`、`Offline` 和 `UnresolvedEffect` 压进一个互斥枚举：

```text
Lifecycle:          Active | Archived | Deleted
Execution:          Opening | Ready | Working | WaitingForUser
                    | Stopping | Complete | Interrupted
Runtime attachment: Detached | Attaching | Attached
Availability:       Local | ReadOnlyElsewhere | Offline | Corrupt
Attention flags:    Unread | UnresolvedEffect | ConfigPending | RecoveryNeeded
```

Rail 的单行徽标只是这些维度的派生视图，展示优先级为：

```text
UnresolvedEffect > Corrupt/Offline > WaitingForUser > Unread
> Interrupted > Stopping > Working > Attaching/Opening > Complete > Ready
```

高优先级徽标不得覆盖底层事实；`/status` 和 Session 详情必须同时显示完整维度，例如“已归档 · 上次运行中断 · 有未确认副作用”。

### 4.3 Surface、Overlay stack 与返回焦点

```text
Base surface: Workbench | Sessions

OverlayFrame
├── kind: CommandPalette | Settings | ModelPicker | Authentication
│          | Diagnostics | Confirmation | Recovery
├── state: 该 Overlay 的选择、搜索、滚动与表单状态
└── return_focus: 打开它之前的稳定 FocusToken

overlay_stack: Vec<OverlayFrame>
```

输入路由遵循以下契约：

1. 终端 resize、挂起恢复等生命周期事件先处理；
2. 栈非空时，仅栈顶 Overlay 接收普通按键；
3. 栈为空时，由当前 Base surface 的焦点控件接收；
4. 未被消费的按键才进入全局命令；`Ctrl-C` 始终发起退出意图，但有运行任务时只 push 退出确认 Overlay；
5. 单字母快捷键在文本输入、搜索框和 Composer 聚焦时绝不抢占字符输入。

Push Overlay 时保存当前 `FocusToken`；Pop 时先恢复该 token。若原控件因配置刷新、列表删除或尺寸变化已不存在，则依次回退到父 Overlay 的首个可用控件、当前 Session Composer、Workbench 的主要动作。后台事件只能使 token 失效并触发上述回退，不能自行移动焦点。

`Esc` 只 pop 栈顶；因此从 Settings 打开的 Model Picker 按一次 `Esc` 回到原设置字段，再按一次才关闭 Settings。Confirmation 和 Recovery 也是普通 OverlayFrame，不拥有绕过栈的第二套焦点规则。

## 5. 主工作台

### 5.1 宽屏：终端宽度 ≥ 110 列

沿用当前 28 列 Session rail 与主对话区域：

```text
 BONE  5                   ┃ 我已经检查了当前启动流程。
                           ┃
 Conversations             ● 当前配置和登录都发生在 TUI 之前，
▌ 1  配置与 Session          所以用户无法在界面中修复。
     • Working             │
  2  工具注册               ✓ Read crates/bone-app/src/main.rs
     ● New activity        │
  3  PRD                    • Thinking · Designing setup states
     ✓ Complete            │
  4  登录问题              │
     ? Waiting for you     │
  5  新对话                │
     · Ready               │
                           ╭─────────────────────────────────────────────╮
 ^N New conversation      │ 继续把启动流程拆成状态机…                  │
                           ╰─────────────────────────────────────────────╯
 ~/code/acme                gpt-… · High    Working · Esc stop · / commands
```

布局规则：

- Rail 固定 28 列，主区域保留 2 列间距；
- Rail 只展示当前 Workspace 的 Session；
- Workspace 友好路径在页脚持续可见；
- 当前模型位于 Composer 附近；
- 当前任务活动行紧贴当前 turn；
- 背景 Session 完成只更新 rail，不切页；
- Session 切换保留各自草稿和滚动位置；
- 当前选择和键盘焦点是两个不同状态；
- Rail 获得焦点时使用更强的选中边缘。

状态文案：

```text
· Ready
… Opening
• Working
? Waiting for you
◌ Stopping
✓ Complete
● New activity
! Interrupted
! Offline
! Unresolved effect
```

最终实现只选用经过 Unicode 宽度测试的符号，文字标签不得省略。

### 5.2 窄屏：终端宽度 < 110 列

```text
 BONE  2/5 ●  ·  配置与 Session                       • Working

 ┃ 继续分析配置系统

 ● 当前需要把配置读取从启动前置条件中移开。

 ✓ Read crates/bone-config/src/manager.rs

 • Thinking · Designing settings flow

 ╭────────────────────────────────────────────────────────────╮
 │ 继续…                                                      │
 ╰────────────────────────────────────────────────────────────╯
 ~/code/acme              gpt-… · Esc stop · /sessions
```

窄屏 Header 中的 `2/5 ●` 是可聚焦的“对话入口”：`Enter` 打开全屏 Session 列表。`/sessions` 是所有终端都必须支持的可靠入口；`Ctrl-Left` 是探测到终端支持时显示的加速键，不能成为唯一入口。

```text
 BONE  5

 Conversations

   1  配置与 Session
      • Working

 ▌ 2  工具注册
      ● New activity

   3  PRD
      ✓ Complete

 ↑↓ 选择     Enter 打开     Esc 返回     / 搜索
```

在没有鼠标、方向组合键被终端拦截或仅 40 列宽时，用户仍可通过 `/sessions`、Header 的 `Enter` 和列表中的 `Esc` 完成往返。Header 始终显示总 Session 数，以及按 `!` 风险、`?` 待用户、`●` 新活动、`…` 运行中的顺序汇总的最高优先级后台标记。

### 5.3 空 Session

Ready：

```text
                    想从哪里开始？

             描述一个任务，或者问 BONE 一个问题。

 ╭────────────────────────────────────────────────────────────╮
 │ Ask BONE…                                                  │
 ╰────────────────────────────────────────────────────────────╯
 ~/code/acme                  gpt-… · Enter send · / commands
```

尚未认证：

```text
                 连接模型账号后即可开始

               [Enter 连接 ChatGPT]   [/config 设置]

 ╭────────────────────────────────────────────────────────────╮
 │ 你仍然可以先写下任务…                                      │
 ╰────────────────────────────────────────────────────────────╯
 ~/code/acme                    未连接 · /login · / commands
```

Composer 必须一直存在，用户可以在设置完成前形成草稿。

### 5.4 未配置工作台

没有用户配置、账号或模型时，BONE 仍先显示完整 App Shell，而不是用欢迎页替代工作台。后台自动创建最小的用户配置目录、数据目录、WorkspaceRegistry 项和 Draft SessionRecord；不会在当前工作目录创建 `.bone/`，也不会要求用户知道这些文件的位置。

```text
 BONE  1/1  ·  新对话                              未连接

                 先描述任务，也可以先完成设置

          Enter 连接 ChatGPT      /config 打开设置

 ╭────────────────────────────────────────────────────────────╮
 │ 修复当前项目的启动配置…                                    │
 ╰────────────────────────────────────────────────────────────╯
 ~/code/acme              草稿已保存 · /login · / 打开命令
```

未配置状态的可用能力：

- 可查看当前 Workspace、历史 Session、诊断和帮助；
- 可新建逻辑 Session、编辑并持久化草稿；
- `/config`、`/status`、`/workspace`、`/sessions`、`/login`、`/help`、`/exit` 可用；
- 不创建 `AgentHost`，不附着任何 Agent Runtime；
- 用户提交消息时才进入“先连接，再发送”的明确流程；
- SessionStore 不可写时 Composer 可保留内存草稿，但发送被禁用并显示恢复动作，不能让未落盘消息开始执行。

## 6. 首次启动与认证

### 6.1 短暂 Bootstrap

只有初始化超过约 300 ms 时显示，避免快速启动时闪烁：

```text
 BONE

 正在准备工作目录…
 ~/code/acme
```

### 6.2 首次欢迎

首次欢迎是工作台上 push 的可关闭 Overlay，不是 TUI 之前的向导。选择“稍后设置”只 pop 欢迎层，底下的 Draft Session、Composer 和 Workspace 导航已经存在。

```text
┌──────────────────────────────────────────────────────────────┐
│ BONE                                                         │
│                                                              │
│ 当前工作目录                                                 │
│ ~/code/acme                                                  │
│                                                              │
│ BONE 会把这个目录作为本次工作的边界。                        │
│ 可用文件操作取决于你启用的工具和权限。                       │
│ 每个对话都会固定绑定到这里。                                 │
│                                                              │
│ 开始前，请连接你的模型账号                                   │
│                                                              │
│   ChatGPT                                                    │
│   使用浏览器安全登录                                         │
│                                                              │
│                 [ Enter  连接 ChatGPT ]                       │
│                                                              │
│ /help 帮助      S 稍后设置      Ctrl-C 退出                  │
└──────────────────────────────────────────────────────────────┘
```

要求：

- 不展示“创建配置文件”；
- 不要求选择文件路径；
- 不要求手工输入模型；
- 不展示冗长多步向导；
- 主要操作只有“连接 ChatGPT”；
- “稍后设置”允许查看历史和设置，但不能假装模型已可用；
- 当前工作目录清晰可见；
- 只有已注册的工具能力可以出现在说明里；只读版本不能宣称会修改文件。

### 6.3 Device Login Overlay

```text
┌────────────────────── 连接 ChatGPT ──────────────────────────┐
│                                                              │
│  1. 在浏览器中打开                                           │
│     https://…                                                │
│  2. 如果页面要求，请输入                                     │
│                                                              │
│                       ABCD-EFGH                              │
│                                                              │
│  不要把这个代码发送给其他人。                                │
│                                                              │
│  • 正在等待浏览器授权…                                       │
│                                                              │
│  C 复制代码    O 打开浏览器    Esc 取消                      │
└──────────────────────────────────────────────────────────────┘
```

行为：

- Overlay 必须显示认证服务实际返回的完整 URL 和 code；不能拼接、猜测或只显示不可验证的短域名；
- 收到 URL/code 后才尝试调用系统浏览器。只有 opener 明确成功时才把第一行改为 `✓ 已在浏览器打开`；失败时显示 `! 未能自动打开，请手工打开上面的地址`，不能谎称“已打开”；
- `O` 仅在存在有效 URL 时可用。`C` 仅在剪贴板能力可用时显示；否则代码仍可通过终端选择复制，并显示“剪贴板不可用”；
- 只有认证服务返回可信 `expires_at` 时才显示倒计时；没有过期信息时显示“正在等待授权”，不能伪造 `04:32` 一类时间；
- 授权状态机至少区分 `RequestingCode`、`AwaitingAuthorization`、`Cancelling`、`Succeeded`、`Expired` 和 `Failed`，轮询不能阻塞 TUI；
- `Esc` 立即 pop 当前 Overlay、恢复原焦点并撤销本次 `attempt_id`。后台取消异步进行；该 attempt 的迟到成功、迟到失败和 credential 响应一律丢弃且不得提交；
- Device code、token 和 authorization header 只存在于认证任务所需的最短生命周期，不进入普通配置、Session journal、transcript、诊断导出或日志；
- 代码过期后停止轮询并原地提供 `[Enter 获取新代码]`；只有用户确认才创建新的 attempt，旧 code 立即清除；
- 登录成功后自动加载模型，不强迫用户额外完成模型向导；
- 用户随时可通过 `/model` 修改推荐值。

### 6.4 未登录时发送

用户可以先写任务。按 Enter 后：

```text
需要连接模型账号才能开始这个任务。
你的任务已保留，登录成功后将自动开始。

[Enter 连接 ChatGPT]    [Esc 返回编辑]
```

只有用户已经确认发送并选择“连接 ChatGPT”时，文本才作为 `pending_submission` 保存在 SessionRecord 中；它尚不是已接受的 User Turn。认证和模型准备成功后，系统再次核对 attempt 与当前 Session，再将消息 durable 为 User Turn 并惰性附着 Runtime。取消、过期或失败时清除 pending 标记并恢复完整草稿，绝不由迟到登录事件自动提交。

### 6.5 认证或网络失败

```text
┌────────────────────── 无法连接 ChatGPT ──────────────────────┐
│                                                              │
│ 网络连接失败。BONE 和你的对话仍然安全保留。                  │
│                                                              │
│ [Enter 重试]                                                 │
│ [D 查看诊断]                                                 │
│ [Esc 稍后再试]                                               │
│                                                              │
│ 错误代码：NETWORK_TIMEOUT                                    │
└──────────────────────────────────────────────────────────────┘
```

默认信息不能包含 endpoint、credential path 或底层错误链。

### 6.6 退出账号与切换账号

`/logout` 是用户级高影响操作，先显示受影响的当前和后台 Session 数量。它不会删除 SessionRecord、历史或草稿。

- 没有在途请求时：停止创建新请求，释放连接，安全删除凭据，再广播“需要登录”；
- 有在途请求时：提供“等待请求完成后退出”“停止请求并退出”“取消”三个明确选项；
- credential 删除失败时保持当前连接和有效状态，显示失败原因与重试；不能先把 UI 画成“已退出”；
- 退出或账号 revision 变化后，其他进程不得静默继续用旧账号创建新请求；已发出的请求按连接语义完成或失败，并给出可见状态；
- 重新登录或切换账号必须建立新连接并完成预检后才原子替换旧连接。

## 7. Command Palette

### 7.1 唤起与过滤

Composer 的第一处非空字符由用户逐键输入为 `/` 时，在 Composer 上方打开候选：

```text
╭ Commands ───────────────────────────────────────────────────╮
│ /mo█                                                        │
├─────────────────────────────────────────────────────────────┤
│ > /model         选择当前对话使用的模型                     │
│   /model default 设置这个工作目录的默认模型                 │
│   /status        查看当前对话和连接状态                     │
├─────────────────────────────────────────────────────────────┤
│ ↑↓ 选择    Enter 打开    Tab 补全    Esc 关闭               │
╰─────────────────────────────────────────────────────────────╯
╭─────────────────────────────────────────────────────────────╮
│ /mo█                                                        │
╰─────────────────────────────────────────────────────────────╯
```

规则：

- 按命令名、别名和本地化说明模糊搜索；
- 最相关优先，其次最近使用，再其次其他命令；
- 复杂命令打开专用 picker；
- `Enter` 打开或执行，`Tab` 只补全；
- `Esc` 关闭并保留输入；
- slash command 不进入模型上下文；
- 未知命令不作为 Prompt 发送；
- 本地命令结果可以形成不进模型上下文的紧凑系统事件。

未知命令：

```text
没有命令 “/modle”

你是不是想用：
  /model    选择模型
```

### 7.2 语法、转义与粘贴安全

命令只在 Composer 的**第一处非空字符**满足以下条件时解析：该字符是用户逐键输入的 `/`，或用户显式打开 Command Palette 后选择了命令。解析器不是 Shell，固定语法为：

```text
command       := "/" name (space argument)*
name          := ASCII 小写字母、数字和 "-"
argument      := bare | "double quoted" | 'single quoted'
literal slash := "//" remainder
```

- 空白分隔参数；引号只用于保留空白；反斜杠只转义下一个引号、反斜杠或空白字符；
- 不执行 `$VAR`、`~`、glob、管道、重定向、命令替换或注释；所有参数只是传给 typed command registry 的 UTF-8 字符串；
- 未闭合引号、未知参数或不合法 scope 留在 Composer 中并显示行内错误，不执行部分命令；
- 要向模型发送以 `/` 开头的普通文本，用户输入 `//请解释这个路径`；发送前只移除第一个 `/`，模型收到 `/请解释这个路径`；
- 前导空格不绕过命令或转义判断；`  /status` 仍是命令，`  //status` 仍是普通文本；
- `/model` 打开当前 Session 的模型选择器；`/model <exact-id>` 只有在目录中精确匹配时才提交，否则打开以该文本过滤的 picker 且不修改设置；
- `/model default [<exact-id>]` 作用于当前 Workspace，`/model global [<exact-id>]` 作用于 User，`/model inherit` 删除当前 Session override；省略 ID 时均打开相应 scope 的 picker；
- `/model search <query>` 始终只打开带初始过滤词的 picker，解决 query 与模型 ID 的歧义；
- `/config get|set|reset|doctor` 是高级入口，仍使用相同 descriptor、scope、校验、确认和事务，不能绕过设置中心的安全规则。

粘贴是独立输入类型，不能被当成一串可信键击：

1. `Event::Paste` 只插入内容；粘贴负载内的换行绝不映射为“提交”，也不自动打开 Palette；
2. 多行粘贴始终作为普通 Prompt 草稿，不能执行其中任意一行的本地命令；
3. 单行粘贴若第一处非空字符为 `/`，按 Enter 时显示确认，默认选中“作为普通文本发送”；只有用户显式选择“解析为本地命令”才允许执行；
4. 用户从粘贴文本中删除并重新逐键输入开头的 `/` 后，可恢复普通命令预览；
5. 若终端无法提供 bracketed-paste 事件，BONE 进入保守模式：slash 命令只能由 Palette 选择或 `Tab` 补全后执行，并在状态栏说明原因。

```text
检测到粘贴的 “/model …”

> 作为普通文本发送
  解析为本地命令

Enter 确认    Esc 返回编辑
```

### 7.3 命令分组

```text
Session
  /new
  /sessions
  /resume
  /rename
  /archive
  /stop

Models & Settings
  /model
  /config
  /status

Account
  /login
  /logout

Workspace
  /workspace

Help
  /help
  /doctor
  /exit
```

### 7.4 忙碌时策略

| 类型 | 例子 | 行为 |
| --- | --- | --- |
| 本地即时 | `/help`、`/status`、`/sessions` | 立即执行 |
| 安全边界 | `/model`、Agent 设置 | 立即保存，显示下一生效点 |
| 工具边界 | 工具限制、权限 | 下一个尚未开始的调用使用 |
| 任务控制 | `/stop` | 立即请求停止当前 Session |
| 当前不可用 | 归档运行中的 Session | 保持可见，说明为何禁用 |

不可用示例：

```text
/archive    当前对话仍在运行；停止后可以归档
```

## 8. Settings Center

### 8.1 打开与作用范围

`/config` 或可选快捷键 `Ctrl-,` 打开设置中心。

宽屏：

```text
╭ BONE 设置 ───────────────────────────────────────────────────╮
│ 搜索设置  /                                                  │
│                                                              │
│ 正在设置：当前工作目录  ~/code/acme                  [S 切换] │
├──────────────────────┬───────────────────────────────────────┤
│ > 模型               │ 模型                                  │
│   Agent 行为         │                                       │
│   工具与权限         │ 默认模型                              │
│   外观               │   gpt-…                         Enter │
│   账号与连接         │   继承自“所有工作目录”                │
│   高级               │                                       │
│   诊断               │ 推理强度                              │
│                      │   High                          Enter │
│                      │                                       │
│                      │ Coordinator 模型                      │
│                      │   自动选择                      Enter │
│                      │   仅高级协调任务会使用                │
├──────────────────────┴───────────────────────────────────────┤
│ ↑↓ 移动  Enter 修改  Space 开关  S 范围  R 继承  Esc 返回    │
╰──────────────────────────────────────────────────────────────╯
```

默认范围：

- 从 `/config` 打开：当前工作目录；
- 从某个 Session 的行内操作打开：仅当前对话；
- 全局范围必须显式切换并在提交前显示影响摘要；
- 入口默认值只能在该字段的 `allowed_scopes` 内取最窄合法范围；字段不支持的范围不显示，也不能通过命令绕过。

显示“继承自所有工作目录”而不是 `source=user`。按 `R` 删除当前范围覆盖并恢复继承。

作用域与继承是字段 descriptor 的产品契约：

| 设置 | Session | Workspace | User | 默认与边界 |
| --- | --- | --- | --- | --- |
| Solver 模型、推理强度 | 允许 | 允许 | 允许 | `/model` 默认 Session；下一 User Turn |
| Coordinator、Agent 默认行为 | 不允许普通 Session 覆盖 | 允许 | 允许 | 默认 Workspace；下一 User Turn |
| 模型超时 | 允许 | 允许 | 允许 | 下一 User Turn 的模型 job |
| 工具开关、限制 | 按 capability | 按 capability | 按 capability | 通常 Workspace；下一次尚未开始的工具调用 |
| 权限策略 | 仅允许收紧父级 | 允许 | 允许 | 收紧立即阻止后续调度；放宽从下一工具调用起 |
| 主题、语言、键位、布局 | 不允许 | 不允许 | 允许 | 下一帧；User 级持久化 |
| 进度详情 | 允许 | 允许 | 允许 | 下一帧；入口使用最窄合法范围 |
| Account、credential、Provider 连接 | 不允许 | 不允许 | 允许 | 新连接准备完成后原子切换，必须确认影响 |
| Workspace root | 只读 | 只读 | 只读 | 当前实例固定，不提供修改或 `/cd` |
| 标题、归档状态 | Session 元数据 | 不适用 | 不适用 | 由 Session 命令修改，不属于 Config |

解析优先级固定为 `Session > Workspace > User > built-in defaults`。`S` 只在当前字段拥有两个以上合法 scope 时出现并循环这些 scope；切换 scope 只改变编辑目标，不复制当前值。`R` 删除当前 scope 的 override，预览继承后值和来源，再作为一笔配置事务提交。安全子 scope 可以进一步收紧父级限制，不能放宽超过父级上限。

### 8.2 分类

设置项必须由运行时 capability registry 的 descriptor 驱动。只有真实注册、可校验且存在 apply consumer 的能力才显示；下面是信息架构示例，不是允许硬编码的功能清单。

```text
模型
  默认模型
  推理强度
  Coordinator 模型（高级）
  模型超时

Agent 行为
  自动压缩上下文
  完成提醒
  进度显示

工具与权限
  当前已注册工具
  工具限制
  默认权限模式

外观
  主题
  语言
  进度详情
  Session rail
  状态栏

账号与连接
  当前账号
  登录 / 退出
  连接状态
  重新连接

高级
  Coordinator
  调试输出

诊断
  设置健康状态
  Session 存储
  凭据存储
  连接测试
  复制脱敏诊断
```

P0 只有当前 ChatGPT Provider 时，Provider 与 Endpoint 在诊断中只读展示，不提供一个无效的切换控件。未来只有 ConnectionManager 注册了“可准备、验证、原子替换”的连接 profile descriptor 后，才可在高级设置中显示可编辑 Provider/Endpoint。未实现的写工具、主题或权限模式同理：隐藏或明确标记为不可用，不能保存一个没有消费者的值。

### 8.3 窄屏

小于 90 列时采用 drill-down：

```text
 BONE 设置                         当前工作目录

 > 模型
   Agent 行为
   工具与权限
   外观
   账号与连接
   高级
   诊断

 ↑↓ 选择    Enter 打开    / 搜索    Esc 关闭
```

进入分类：

```text
 设置 / 模型

 > 默认模型              gpt-…
   推理强度              High
   Coordinator 模型      自动选择
   模型超时              120 秒

 ↑↓ 选择    Enter 修改    R 恢复继承    Esc 返回
```

### 8.4 自动保存与反馈

一次完整选择是一笔事务。Picker 选项在按 Enter 前只是本地 candidate；按 Enter 后立即开始，不需要总保存按钮：

```text
Candidate（仅 UI）
→ Validating（schema、scope、capability、账号）
→ Preparing（新模型/连接/策略资源；旧 Effective 仍服务）
→ Persisting Desired revision（原子写入）
→ Applying now | Pending safe boundary
→ Runtime acknowledgement(effective_revision)
→ Applied + promote Last Known Good
```

三个值必须能同时解释：`Desired` 是用户刚确认的目标，`Effective` 是当前运行时实际使用的值，`Last Known Good` 是最近一次持久化且成功应用的完整配置。字段详情在三者不一致时必须展开显示差异，不能只画一个绿色勾。

字段状态：

```text
默认模型
  gpt-…                 ◌ 正在验证…
```

```text
默认模型
  gpt-…                 ✓ 已应用
```

```text
默认模型
  new-model             ◌ 已保存 · 下一条消息生效
  当前任务：old-model
```

```text
默认模型
  gpt-previous

  ! 这个模型不适用于当前账号。
    原来的模型仍在使用。            [Enter 重新选择]
```

要求：

- validation 或 prepare 失败时不持久化 Desired，继续显示并运行旧 Effective；
- 持久化成功后只能执行预先准备好的不可失败原子句柄替换；若某类 consumer 不能保证这一点，则保留 `desired_revision != effective_revision`，显示“已保存，尚未应用”，并继续使用 LKG；
- apply acknowledgement 必须携带 effective revision；只有 revision 与最新 Desired 一致时才显示“已应用”；
- 应用失败时自动回到 LKG 作为 Effective。若 Desired 已落盘，存储必须保留明确的未应用/修复状态或执行补偿回滚，UI 要分别说明“未保存”与“已保存但未应用”；
- 边界等待期间连续修改以最后一次已验证 Desired 为目标，旧 pending 显示“已被更新的选择替代”，迟到 acknowledgement 不得覆盖新状态；
- 主题等纯 UI 设置可以在下一帧预览，但只有持久化成功后才标记“已保存”；失败时恢复旧主题并保持焦点；
- 不丢焦点；
- 不只显示 Provider 原始错误；
- 成功操作提供短时 `U 撤销`；撤销是以旧值发起的新事务，不是绕过校验的内存回退；
- 选择已提交后 `Esc` 可以关闭 Overlay，远程事务继续；仍在编辑但未确认的文本 candidate 按 `Esc` 丢弃 candidate 并恢复字段值；
- 远程验证可在 Overlay 关闭后继续，结果以 Toast 返回；
- 设置中心打开时 Session 继续运行。

## 9. Model Picker

### 9.1 默认界面

设计稿中的具体模型名称仅用于表达列表层级和切换状态；实现不得把这些名称当作静态可用列表，生产内容必须来自当前账号的模型目录或明确标记的版本化推荐 catalog。

```text
╭ 选择模型 ────────────────────────────────────────────────────╮
│ 搜索模型  gpt█                                               │
│                                                              │
│ 应用于：● 当前对话   ○ 当前工作目录   ○ 所有工作目录         │
├─────────────────────────────────────────────────────────────┤
│ 推荐                                                         │
│ > gpt-…                当前 · 平衡                           │
│   gpt-…                更快                                  │
│   gpt-…                深度推理                              │
│                                                              │
│ 其他可用模型                                                 │
│   …                                                          │
├─────────────────────────────────────────────────────────────┤
│ gpt-…                                                        │
│ 适合：日常编码与复杂任务                                     │
│ 推理：High 可用                                              │
│                                                              │
│ ↑↓ 选择   Enter 应用   Tab 切换范围   / 搜索   Esc 取消      │
╰─────────────────────────────────────────────────────────────┘
```

设计要求：

- 默认作用于当前对话；
- 模型来自当前账号的真实目录；
- 当前模型有“当前”文字标记；
- 能力和推荐标签只来自可靠 metadata；
- 不展示无法保证准确的价格或营销评分；
- 手工模型 ID 是高级入口；
- Coordinator 不混入普通模型列表；
- `/model <id>` 只在 exact match 时直接提交；无 exact match 时把文本作为初始过滤词打开 picker，明确显示“未更改”；
- `/model search <query>` 始终只搜索；`/model inherit` 恢复继承；
- 每一项显示可信来源：`已验证`、`已验证于 <时间>` 或 `尚未验证`，版本化候选和手工 ID 不能冒充当前账号目录；
- 切换到 Workspace/User scope 时，在应用前显示会受影响的继承 Session 数量；有 Session override 的对话不计入。

### 9.2 空闲时切换

成功后关闭选择器，状态栏更新，显示：

```text
✓ 当前对话已切换到 gpt-…
```

可记录本地时间线事件：

```text
— 模型已切换：old-model → new-model
```

该事件不进入模型上下文。

### 9.3 运行中切换

```text
✓ 已选择 new-model

当前任务仍使用 old-model。
你发送下一条消息时将使用 new-model。
```

状态栏在安全边界前显示：

```text
old-model → new-model · 下一条消息生效
```

运行时确认后：

```text
new-model · High
```

一个 User Turn 内的模型调用、工具定义、Coordinator 和 Agent 行为使用同一份 `TurnConfig`，不在任务中途混用 revision。生效前连续切换多次时，只保留最后一次已验证选择；迟到的旧验证结果不能覆盖它。

### 9.4 Workspace 默认模型

```text
✓ 当前工作目录的默认模型已更新

3 个继承此设置的对话将在各自下一条消息使用 new-model。
2 个有独立模型设置的对话保持不变。

[Enter 查看这些对话]
```

### 9.5 模型目录失败

有缓存：

```text
! 暂时无法刷新模型列表

下面显示 2 小时前加载的可用模型。
[R 重试]    [L 重新登录]
```

无缓存：

```text
无法获取可用模型

[Enter 重试]
[L 重新连接账号]
[D 查看诊断]
```

目录和提交错误必须区分并提供不同恢复动作：

| 错误 | 用户文案与动作 | Effective 行为 |
| --- | --- | --- |
| 登录过期 | `登录已过期`；重新登录、取消 | 保留 LKG，禁止新请求 |
| 模型不存在 | `目录中没有此模型`；返回搜索 | 原模型继续 |
| 账号无权限 | `当前账号不能使用此模型`；选择其他模型 | 原模型继续 |
| reasoning 不兼容 | 展示此模型支持的选项 | 不静默降级 |
| 限流 | 显示可重试状态和服务提供的 retry 时间 | 不改配置 |
| 网络/服务故障 | 重试、使用可信缓存或诊断 | 不把未验证候选标成可用 |

不得用一次会计费的普通生成请求伪装成模型 listing。无 authoritative listing 时，版本化 catalog 只能提供候选，提交前仍走 Provider 支持的能力检查。

## 10. 实时配置反馈规范

产品定义：

> 用户确认设置后，无需离开 TUI、重启 BONE、重建 Session 或手工 reload；系统立即开始校验和应用，并持续显示真实状态。

| 状态 | 标准文案 | 运行时语义 |
| --- | --- | --- |
| Candidate | `正在编辑` | 仅 UI 草案，未提交、未持久化 |
| Validating | `◌ 正在验证…` | schema、scope 与依赖校验；旧 Effective 有效 |
| Preparing | `◌ 正在准备…` | 创建替代资源；旧模型/连接/策略仍服务 |
| Saved | `✓ 已保存 · 正在应用` | Desired 已原子持久化，尚未收到 runtime ack |
| Pending next turn | `✓ 已保存 · 下一条消息生效` | 当前 Turn 保持旧 TurnConfig，新 revision 等待边界 |
| Pending tool boundary | `✓ 已保存 · 下一次工具调用生效` | 只影响尚未开始的工具调用 |
| Applied | `✓ 已应用` | consumer 已确认同一 effective revision，且 LKG 已提升 |
| Failed before save | `! 未保存 · 原设置仍在使用` | candidate 丢弃，持久值和 Effective 都未改变 |
| Failed after save | `! 已保存但无法应用 · 仍在使用原设置` | Desired 与 Effective 明确分离，LKG 继续服务并进入修复态 |

反馈层级：

- 字段状态：紧邻设置项；
- 普通成功：3 秒非抢焦点 Toast；
- 当前 Session 重要变更：可写入本地时间线事件；
- Workspace/全局变更：Toast 说明受影响 Session 数量；
- 失败或持续降级：保留 Banner，直到恢复；
- 后台验证完成：只 Toast，不关闭当前 Overlay。

安全边界必须按字段显示，不使用含糊的统一“下一轮”：

| 设置 | 生效点 | 在途工作 |
| --- | --- | --- |
| 主题、布局、语言、进度显示 | 当前或下一帧 | 预览失败则回退 |
| Solver、reasoning effort、Coordinator、Agent 行为、模型超时 | 下一条用户消息创建的新 User Turn | 当前 Turn 全程继续使用旧 TurnConfig |
| Workspace/User 模型默认值 | 每个继承 Session 的下一 User Turn | Session override 不变 |
| 工具开关与普通限制 | 下一次尚未开始的工具调用 | 已开始调用不伪装成被修改 |
| 权限收紧 | 立即阻止后续调度 | 已开始调用按取消能力显示真实状态 |
| 权限放宽 | 下一次工具调用 | 必须先完成确认与持久化 |
| Account/连接 | 新连接准备成功后的原子 swap | 旧连接服务到 swap；已发请求不迁移 |

配置 revision 的进度按 scope 和 consumer 跟踪。Workspace 变更影响多个 Session 时，Toast 可显示 `已保存：空闲 3 个将在下条消息使用 · 运行中 2 个待当前任务结束`；这不是失败或部分保存，而是各自安全边界不同。Idle/Detached Session 无需为了制造 ack 创建 Runtime，也不能被称为“模型已切换”；它们在下一 Turn 解析最新 Effective revision，实际调用确认后才形成 runtime 使用记录。

重连：

```text
◌ 正在重新连接 · 现有对话和草稿已保留
```

失败：

```text
! 新连接不可用，已继续使用原连接
```

连接切换失败时新凭据或连接 profile 不得成为 Effective。若 Desired 已持久化，UI 保持可见的“待修复”状态，并提供 `[R 重试] [K 恢复原设置] [D 诊断]`；选择恢复原设置走补偿事务。旧连接不可用时不能使用“继续使用原连接”，而要准确进入 `未连接` 并保留 Session 数据。

## 11. 配置错误恢复

### 11.1 单字段无效

```text
 模型                                           !

 默认模型
   model-does-not-exist

   ! 这个模型对当前账号不可用。
     原来的有效模型 gpt-… 仍在使用。

   [Enter 选择可用模型]    [R 恢复继承]
```

### 11.2 整份设置损坏

```text
┌────────────────────── 设置需要修复 ──────────────────────────┐
│                                                              │
│ BONE 无法读取之前保存的设置。                                │
│ 你的工作目录和历史对话没有受到影响。                         │
│                                                              │
│ 当前正在使用最近一次可用设置。                               │
│                                                              │
│ [Enter 自动修复并继续]                                       │
│ [T 本次使用临时设置]                                         │
│ [D 查看技术详情]                                             │
│                                                              │
│ 自动修复会先保留原设置的备份。                               │
└──────────────────────────────────────────────────────────────┘
```

有 LKG 时优先使用 LKG；没有 LKG 时才使用 built-in default，并且工具、网络和权限 fail closed。自动修复前必须保留原始损坏内容或备份，修复不得扩大权限。用户不需要寻找文件。

### 11.3 无法写入

```text
! 设置未保存

BONE 暂时无法保存设置。
原来的设置仍在使用。

[R 重试]    [D 查看诊断]    [Esc 继续使用]
```

临时模式页脚：

```text
临时设置 · 退出后不会保留
```

不能让内存新值看起来像已经持久化。

这里的“设置未保存”只表示 ConfigStore 失败。若 SessionStore 仍可写，当前 Effective 配置可以继续完成已有工作；若 SessionStore 也不可写，则不得接受或执行新消息，Composer 只保留当前进程内草稿，并显示 `历史暂时无法保存 · 发送已禁用`。

### 11.4 多实例冲突

系统先自动重新读取并重试一次。同字段确实冲突时：

```text
这个设置刚刚在另一个 BONE 窗口中发生了变化。

当前值：High
你的选择：Medium

[Enter 使用我的选择]
[K 保留当前值]
```

不展示 revision number。

### 11.5 存储与迁移异常

| 异常 | 工作台是否可进入 | 用户仍可做什么 | 禁止行为 |
| --- | --- | --- | --- |
| 配置目录不存在 | 是，后台自动创建 | 全部本地导航；创建成功后设置 | 要求手工建文件 |
| ConfigStore 不可写 | 是，持续 Banner | 使用 LKG、历史、诊断、重试 | 把 candidate 标成已保存 |
| SessionStore 不可写/磁盘满 | 是，阻断发送 | 查看已有历史、保留内存草稿、诊断、重试 | 接受消息或启动 Runtime |
| 单个 Session journal 损坏 | 是，隔离该 Session | 打开其他 Session、备份/修复损坏项 | 阻断整个 Workspace |
| WorkspaceRegistry 损坏 | 是，Recovery 页 | 恢复备份、只读诊断、安全退出 | 猜测新 ID 后混入旧 Session |
| schema 迁移失败 | 是，RepairNeeded | 使用 LKG、备份、重试或临时只读 | 静默覆盖原配置 |
| credential store 不可用 | 是，显示未连接 | 历史、草稿、设置、诊断 | 把 token 写进普通配置 |

每种错误都显示受影响的存储、哪些数据已持久化、哪些只是内存数据，以及可执行的下一步。技术路径只在用户打开诊断详情后出现。

## 12. Session 浏览、冷恢复与 Workspace 隔离

### 12.1 启动恢复与惰性附着

再次从同一 canonical Workspace 启动时：

```text
加载 Workspace Session 索引
→ 恢复上次选中的 SessionRecord、草稿和滚动位置
→ 按需 hydrate 当前可见历史
→ 将崩溃时 Working/Stopping 的记录标记为 Interrupted
→ 显示工作台
→ 用户提交新消息后才获取可写 lease 并附着 Runtime
```

- 不为 rail 中全部历史 Session 创建 Agent Runtime，也不因浏览历史建立模型连接；
- 没有历史记录时创建可持久化 Draft SessionRecord；账号和模型仍可稍后设置；
- `/resume` 的含义是打开逻辑 SessionRecord，不是恢复旧 Future、网络流或 shell 进程；
- 当前进程已经为该 Session 持有 Runtime 时，重新选择只复用同一个 handle；绝不创建第二个 Runtime；
- 已确认消息必须先 durable 到 journal，成功后才进入 User Turn 和 Runtime；落盘失败时消息回到 Composer。

### 12.2 Session Picker

`/sessions` 和 `/resume` 默认只查询当前工作目录：

```text
╭ 当前工作目录的对话 ─────────────────────────────────────────╮
│ 搜索  config█                                               │
├─────────────────────────────────────────────────────────────┤
│ > 配置与 Session     2 分钟前     • Working                 │
│   首次登录流程       昨天         ✓ Complete                │
│   错误恢复测试       9 月 4 日    ! Interrupted             │
│                                                             │
├─────────────────────────────────────────────────────────────┤
│ Enter 打开   N 新建   R 重命名   A 归档   Esc 返回          │
╰─────────────────────────────────────────────────────────────┘
```

Session rail 中的数字只是当前 UI 快捷索引，不作为持久 Session ID 展示。

打开 Session 只 hydrate 展示数据；标题、归档和草稿属于 SessionRecord 元数据，不依赖 Runtime。

### 12.3 Interrupted 与未知副作用

冷恢复绝不自动继续上次任务，也不自动重放模型或工具调用：

```text
┌────────────────────── 上次任务被中断 ────────────────────────┐
│ 已保存的消息和回复仍在这里。                                 │
│ 进程内任务无法跨启动继续。                                   │
│                                                              │
│ > 在 Composer 中输入下一步                                   │
│   重新提交上一条用户消息                                     │
│   只保留历史                                                 │
│                                                              │
│ Enter 选择    Esc 只保留历史                                 │
└──────────────────────────────────────────────────────────────┘
```

“重新提交”先显示完整消息预览并创建一个新的 User Turn，不复用旧 job id。存在 `UnresolvedEffect` 时禁用重新提交，优先显示哪些工具调用结果未知；用户只能查看记录、确认外部状态、输入有针对性的后续消息或归档。`Interrupted` 在用户发送新消息后作为历史事实保留，但当前 Execution 进入新的 Working/Ready 状态。

### 12.4 同一 Session 已在其他进程运行

拿不到可写 Runtime lease 时可以只读打开：

```text
! 这个对话正在另一个 BONE 窗口中工作

历史会继续刷新，但这里不能发送、停止或修改 Session 设置。
[R 刷新]    [T 请求接管]    [Esc 返回]
```

`T` 必须进入影响确认，并优先请求原进程协作释放。只有 lease 已过期或原进程确认释放后才能接管；不得靠忽略锁建立双写。原进程失联且存在未确认副作用时，接管页必须展示风险并禁止自动重跑。不能实现安全接管的平台只提供只读与返回。

### 12.5 跨 Workspace 拦截

只有用户显式搜索“所有项目”或输入外部 Session ID 时才会遇到：

```text
┌──────────────────── 无法在这里恢复此对话 ────────────────────┐
│                                                              │
│ 这个对话属于另一个工作目录：                                 │
│ ~/code/project-b                                             │
│                                                              │
│ 当前工作目录：                                               │
│ ~/code/project-a                                             │
│                                                              │
│ 为避免在错误项目中操作文件，BONE 不会重新绑定或复制它。      │
│                                                              │
│ [Enter 复制在原目录启动 BONE 的命令]                         │
│ [Esc 取消]                                                   │
└──────────────────────────────────────────────────────────────┘
```

复制内容由平台安全引用器生成，语义仅为“切换到原目录并启动 `bone`”；不包含 secret，不自动执行，也不改变当前进程 cwd。剪贴板不可用时显示可选择的命令文本。vNext 不提供跨 Workspace Fork、rebind 或复制成当前目录的新对话；这些能力在定义消息、工具摘要、附件和未知副作用的迁移契约前不得以快捷键隐藏上线。

## 13. 忙碌与并发交互

### 13.1 打开 Overlay

- 所有 Session 继续运行；
- 宽屏 rail 持续更新；
- Overlay 底部可显示 `后台：2 个对话正在运行 · 1 个有新结果`；
- 完成事件不关闭 Overlay；
- Toast 不接收焦点。

### 13.2 工作中继续输入

Composer 始终可编辑。如果 Runtime 支持 steer：

```text
✓ 已加入当前任务 · 将在下一个安全点处理
```

如果尚不支持 steer，必须准确显示：

```text
已排队 1 条消息    [E 编辑] [X 取消]
```

不能把“排队”表现为“已执行”。真正并行的工作通过 `Ctrl-N` 新建 Session。

Steer 或排队消息不会改变正在运行 User Turn 的 TurnConfig。只有它作为下一条 durable 用户消息启动新 Turn 时，才解析当时最新的 Effective revision。

### 13.3 停止

- 有 Overlay：`Esc` 只关闭最上层 Overlay；
- 无 Overlay 但 Sessions surface 聚焦：`Esc` 返回 Composer；
- 无 Overlay、Workbench/Composer 聚焦且当前 Session 工作中：`Esc` 请求停止；
- 重复 `Esc` 不伪装成已停止；
- 其他 Session 不受影响。

```text
◌ 正在停止 · 2 个操作正在安全收尾
```

完成后：

```text
— 工作已停止
```

### 13.4 新建逻辑 Session 与附着 Runtime

`Ctrl-N` 先创建并持久化一个 Draft SessionRecord，立即插入并选择：

```text
  6  新对话
     · Ready
```

此时 `runtime_attachment=Detached`，不会连接模型。用户可以立刻输入并保存草稿；只有 Enter 确认的消息 durable 后才显示：

```text
消息已保存 · 正在准备对话
… Attaching runtime
```

附着失败时：

- 尚未得到 Runtime 接收回执的消息恢复到 Composer；已 durable 的消息保留为待发送记录，不重复写入；
- 草稿完整保留；
- 只有该 Session 的 Availability 变为 Offline，SessionRecord 仍可打开；
- 提供 `R 重试`；
- 不影响其他 Session。

### 13.5 退出

有运行任务时：

```text
┌────────────────────────── 退出 BONE？ ────────────────────────┐
│                                                              │
│ 2 个对话仍在工作。                                           │
│                                                              │
│ [Enter 停止任务并退出]                                       │
│ [W 等待它们完成]                                             │
│ [Esc 返回]                                                   │
└──────────────────────────────────────────────────────────────┘
```

存在 unknown effect 时必须显示更强警告，不使用模糊的“安全退出”。

退出顺序为：停止接收新的提交 → 处理退出选择 → flush Session journal、草稿和 UI 恢复状态 → 请求 Runtime 收尾 → 恢复终端。配置事务或登录 attempt 仍在后台时，退出确认必须说明它们会被取消；未获得 `Applied` ack 的设置不能在退出摘要中称为已应用。flush 失败时保留 TUI 并提供“重试”与“放弃未保存草稿后退出”，后者必须列出准确的数据损失范围。

### 13.6 交互 TUI 与 one-shot CLI 边界

前端根据参数和终端能力先确定模式，不能在运行中悄悄从 one-shot 切进全屏 TUI：

| 调用 | 模式 | 契约 |
| --- | --- | --- |
| `bone` 且 stdin/stdout 是 TTY | Full-screen TUI | 使用本文 App Shell、Overlay 与 slash command |
| `bone <message>` | One-shot | 不进入 raw/full-screen TUI；创建当前 Workspace 的持久 SessionRecord 后执行一次任务 |
| `bone --model <id> <message>` | One-shot + Session override | `<id>` 必须精确；只 materialize 为该 Session override，不修改 Workspace/User 默认 |
| `bone --events <path> <message>` | One-shot + event export | durable journal 仍是事实来源；events 只是外部导出 |
| `bone` 且不是 TTY、也没有 message | 非交互错误 | 不启动 TUI、不猜测 stdin prompt；提示显式传入 message |

兼容要求：

- one-shot、TUI 共用 cwd canonicalization、WorkspaceRegistry、配置解析、ModelCatalog、SessionStore 和安全策略；
- one-shot 先把 SessionRecord 与用户消息 durable，再附着 Runtime；创建的对话会出现在以后从同一 Workspace 打开的 TUI 中；
- 缺少账号或模型时不内嵌半套登录向导。TTY 输出简洁提示 `请先运行不带消息的 bone，在 TUI 中完成连接` 并非零退出；非 TTY 输出稳定、可解析的错误类别到 stderr；
- 非 TTY 禁止设备登录、浏览器 opener、Overlay、ANSI 控制序列和 secret/code 输出；
- one-shot 参数中以 `/` 开头的 message 是普通 Prompt，不解析本地 slash command；需要传递以 `-` 开头的文本时遵循 CLI 的 `--` 参数分隔规则；
- `BONE_MODEL` 若保留，和 `--model` 一样只生成带来源的初始 Session override，之后可在 TUI 中替换或恢复继承，不能形成隐藏的永久优先级；
- 错误退出必须说明消息是否已 durable、Session ID 是否已创建及能否在 TUI 恢复，但默认不输出内部路径、credential 信息或原始错误链。

## 14. 键盘交互矩阵

### 14.1 全局

| 按键 | 行为 |
| --- | --- |
| `Ctrl-N` | 新建当前 Workspace 的 Session |
| `Ctrl-Left` | 若终端支持，聚焦 rail 或打开 Session 列表 |
| `Ctrl-Right` | 若终端支持，从 Session 列表返回 Composer |
| `Ctrl-,` | 若终端支持，打开设置中心 |
| `Ctrl-C` | 退出；存在运行任务时确认 |
| `/` | 仅在命令位置且来自逐键输入时打开 Command Palette |
| `F1` 或 `/help` | 打开帮助 |

`/sessions`、列表 `Esc` 和 `/config` 分别是上述组合键的强制兼容入口。帮助页必须只展示当前终端已识别的加速键，并始终同时展示 slash/Enter 入口。

### 14.2 Composer

| 按键 | 行为 |
| --- | --- |
| `Enter` | 发送；忙碌时按 Runtime 能力 steer 或排队 |
| `Ctrl-J` | 插入换行 |
| `PageUp/PageDown` | 浏览历史 |
| `Ctrl-Home` | 到最早记录 |
| `Ctrl-End` | 回到实时尾部 |
| `Esc` | 栈非空时 pop 栈顶；栈空且当前 Session 工作中时请求停止；空闲时不丢草稿 |

### 14.3 Session rail/list

| 按键 | 行为 |
| --- | --- |
| `Up/Down` | 选择 Session |
| `Enter` | 打开所选 Session |
| `Ctrl-Right` | 返回 Composer |
| `Esc` | 返回 Composer |
| `Ctrl-N` | 新建 Session |
| `/` | 聚焦列表搜索；`/sessions` 可从 Composer 可靠进入 |

### 14.4 Palette、Picker 与 Dialog

| 按键 | 行为 |
| --- | --- |
| `Up/Down` | 移动选择 |
| `Enter` | 确认 |
| `Tab/Shift-Tab` | 按视觉顺序切换可操作区域；Model Picker 中仅在 scope 控件聚焦时切换范围 |
| `/` | 聚焦搜索 |
| `Esc` | 关闭最上层界面并恢复原焦点 |

### 14.5 Settings Center

| 按键 | 行为 |
| --- | --- |
| `Up/Down` | 移动字段 |
| `Left/Right` | 切换分类或枚举值 |
| `Enter` | 打开 picker 或确认输入 |
| `Space` | 切换开关 |
| `S` | 当前字段支持多个 scope 且无文本框聚焦时切换范围 |
| `R` | 删除当前范围覆盖并恢复继承 |
| `/` | 搜索设置 |
| `Esc` | 返回上级或关闭 |

单字母快捷键只是可见加速键，不是完成主流程的唯一方法。所有动作必须可以通过方向键或 `Tab` 聚焦后按 `Enter` 完成。单字母快捷键只在对应 Overlay 的非文本控件聚焦时生效，必须持续显示提示；Composer、搜索框、手工 ID 和路径文本框中按 `S`、`R`、`C`、`O` 等只输入字符。

## 15. 响应式规则

| 条件 | 行为 |
| --- | --- |
| `width ≥ 110` | 28 列 Session rail + 2 列 gutter + Workbench |
| `width < 110` | Workbench 全宽；Session 使用全屏列表 |
| Settings `width ≥ 90` | 分类与设置项双栏 |
| Settings `width < 90` | 分类 drill-down，再进入字段列表 |
| `width < 48` | 减少横向边距、隐藏非必要说明、保留动作与状态 |
| `< 40×12` | 显示窗口过小提示，保持后台任务、草稿和状态 |

Overlay 不应超出终端，不使用内部横向滚动。长路径中间省略，末两级目录优先保留。

在 40 列布局中，所有 Overlay 变成全屏页面，动作纵向排列：

```text
连接 ChatGPT

打开：
https://…

代码
ABCD-EFGH

• 等待授权

> 打开浏览器
  复制代码
  取消

↑↓ 选择  Enter  确认  Esc 返回
```

- URL 可以换行但不能隐藏 scheme/host；code 不截断、不与倒计时同行；
- Model Picker 先显示搜索与列表，详情按 `I` 或 Enter 前的第二步展开；scope 是独立可聚焦行；
- Session 列表始终从 Header 的“对话 x/y”或 `/sessions` 进入，不依赖不可见 rail；
- Settings、Recovery 和确认页保留问题、数据安全结论和至少一个恢复动作，优先删装饰说明；
- 高度不足时 Composer、当前错误和主要动作优先于历史；resize 回来后恢复原滚动锚点和焦点 token；
- 小于 `40×12` 的提示本身仍提供 `Ctrl-C 退出`，并持续处理后台事件；尺寸恢复后回到原 surface/overlay stack。

## 16. 视觉语言

### 16.1 色彩语义

沿用现有克制风格：

- 蓝色：焦点、当前项、活动中；
- 绿色：成功、完成、已应用；
- 黄色：等待、未读、可恢复风险；
- 红色：确定错误、离线、危险操作；
- 中性色：普通正文、边框和次要信息。

颜色必须配符号和文字，并支持 `NO_COLOR` 与高对比模式。

### 16.2 层级

- 只给当前焦点使用最强边缘或反色；
- 同一屏不出现多个竞争焦点；
- Overlay 使用完整不透明背景，避免底层文本穿透；
- 成功反馈短暂，失败 Banner 持续；
- 不为普通选项堆叠彩色 badge；
- 不使用动画表达关键状态。

### 16.3 文案模板

使用：

```text
当前工作目录
当前对话
所有工作目录
正在验证
已应用
将在下一条消息生效
原设置仍在使用
你的草稿和对话已保留
```

错误固定回答三个问题：

1. 发生了什么；
2. 用户的数据是否安全；
3. 下一步能做什么。

示例：

```text
无法保存模型设置。
原模型仍在使用，你的对话没有受到影响。
请重试或重新连接账号。
```

## 17. 焦点、可访问性与安全

### 17.1 焦点与可访问键盘契约

- 焦点始终唯一并可见；`FocusToken` 使用 surface、Session ID、Overlay ID 和控件稳定 ID，不用易漂移的列表下标；
- Overlay push 保存返回 token，pop 后回到打开前的控件；控件消失时按 4.3 的顺序回退；
- 栈顶 Overlay 是唯一模态层；父 Overlay 保留搜索、选择和滚动状态但不接收输入；
- 后台完成不改变焦点；
- Toast 不接收焦点；
- 列表选择变化不自动执行；
- 危险操作使用明确确认，默认焦点落在保守选项；不绑定易误触的单字符即时执行；
- 所有主流程可纯键盘完成；
- 颜色不是唯一状态载体；
- 关键状态必须有稳定文字；spinner、Unicode 符号、声音和短时 Toast 都不能成为唯一反馈；
- 支持 `NO_COLOR`、高对比和 ASCII fallback；字符宽度异常时优先保留文字标签；
- 动态进度默认更新固定 live region 行，不让每个 tick 追加 transcript 噪声；用户可关闭进度动画；
- 帮助页按当前 surface 列出可用键，并提供命令入口，不要求记忆组合键；
- 粘贴内容不能未经确认触发本地命令；
- token、authorization header 和 device code 不进入 transcript；
- 复制诊断自动脱敏；
- canonical path 用于安全身份，display path 用于友好展示；
- 外部 Session 不因目录 basename 相同而被视为当前 Workspace。

### 17.2 完整异常流

所有 `Validating`、`Preparing`、`Connecting`、`Attaching`、`Stopping` 和 `Cancelling` 都必须最终进入成功、失败、取消或明确的仍在等待状态；超过服务提供的 deadline 时提供重试/取消，不能永久 spinner。异常页统一回答“发生了什么、已保存什么、当前仍使用什么、下一步是什么”。

| 触发 | 可见状态与数据保证 | 主要动作 |
| --- | --- | --- |
| cwd canonicalize 失败 | TUI Recovery；不退回父目录、不创建猜测 Workspace | 重试、诊断、安全退出 |
| Workspace 运行中消失/失权 | App `Degraded`；暂停新工具调度，Session/草稿保留；在途外部效果按真实结果或 Unknown 记录 | 重试访问、查看诊断、停止任务 |
| ConfigStore 不可读/迁移失败 | 使用 LKG；无 LKG 时安全默认且权限 fail closed；原内容保留 | 自动修复、临时只读、诊断 |
| ConfigStore 不可写/CAS 冲突 | Effective 不变；candidate 不称为已保存 | 重试、字段冲突选择、继续用旧值 |
| SessionStore 不可写/磁盘满 | 发送禁用；内存草稿明确标为未保存；不启动 Runtime | 重试、复制草稿、诊断、放弃后退出 |
| 单 Session 损坏 | 仅该项 `Corrupt`，其他 Session 正常 | 备份并修复、归档损坏项、诊断 |
| credential store 不可用/登录过期 | `未连接`；历史和草稿可用，新请求禁用 | 登录、重试、诊断 |
| 登录取消/过期/迟到成功 | attempt 撤销，code 清除，pending submission 回到草稿；迟到事件丢弃 | 重新登录或继续编辑 |
| 浏览器/剪贴板不可用 | URL/code 仍完整可选，不谎称已打开或已复制 | 手工打开/复制、重试 opener |
| 模型目录无缓存且失败 | `NeedsModel`；不启动新 Turn，不伪造可用模型 | 重试、重新登录、诊断 |
| 模型/连接 prepare 失败 | 新 Desired 不成为 Effective；LKG 继续 | 重新选择、重试、恢复 LKG |
| 连接在 Turn 中断开 | 当前 Turn 显示真实失败或 Interrupted；消息与已完成记录 durable | 重连、以新 Turn 重试、保留历史 |
| Runtime attach 失败 | SessionRecord 可用，消息不丢失、不重复；Availability `Offline` | 重试附着、编辑待发消息、诊断 |
| 同 Session 被其他进程持有 | 只读且持续刷新；绝不双写 | 返回、刷新、受控接管 |
| Stop 超时或工具结果未知 | 保持 `Stopping`/`UnresolvedEffect`，不显示“已停止且安全” | 等待、查看详情、强制退出并保留风险记录 |
| 终端缩小或暂时挂起 | 后台继续；surface、Overlay stack、草稿与焦点 token 保留 | 恢复尺寸；必要时 `Ctrl-C` 退出 |
| 进程崩溃后重启 | 已 durable 内容恢复；旧 Working 变 Interrupted；工具不自动重放 | 输入下一步、显式重新提交、只保留历史 |

失败发生在非当前 Session 或被关闭的 Overlay 时，只更新对应 Session flag、持续 Banner 或 Toast；不得切换当前 Session或重新打开 Overlay。用户从通知进入详情后，关闭详情必须回到通知前的焦点。

## 18. 前端事件与状态建议

当前单 UI loop 可以保留。新增异步服务都将结果汇入同一个事件循环：

```text
UiEvent
├── TerminalInput::Key
├── TerminalInput::Paste
├── TerminalResized
├── AgentUpdate { session_id, runtime_generation, update }
├── RuntimeAttachmentUpdate { session_id, state, lease }
├── BootstrapUpdate
├── ConfigUpdate { desired_revision, effective_revision, scope, diff, apply_state }
├── AuthenticationUpdate { attempt_id, state }
├── ConnectionUpdate { connection_revision, state }
├── ModelCatalogUpdate
├── SessionStoreUpdate
└── ToastExpired
```

`attempt_id`、`runtime_generation` 和 revision 用于丢弃取消后或被新操作替代的迟到事件。事件循环是唯一能修改展示状态的 writer；observer、ConfigService、认证轮询和 Runtime task 不能直接改焦点、push Overlay 或绘制。

本地控制反馈使用独立的 `LocalSessionEvent` 流：

```text
LocalSessionEvent
├── SettingChanged
├── ModelChanged
├── SessionResumed
├── WorkStopped
└── RecoveryNotice
```

它可以与 Agent record 合并渲染为时间线，但不写进模型可见消息、Turn transcript 或 prompt history。导出事件时也要用类型字段区分，不能把本地配置文本伪装为 Assistant 消息。

建议状态拆分：

```text
App
├── shell_phase / setup_state / connection_health / storage_health
├── workspace: WorkspaceContext
├── session_records: SessionRecordIndex
├── session_views: Map<SessionId, SessionPresentation>
├── runtime_attachments: Map<SessionId, RuntimeAttachment>
├── current_session: SessionId
├── surface: Surface
├── overlay_stack: Vec<OverlayFrame>
├── focus: FocusToken
├── notifications
├── connection_state
├── config_health
└── pending_operations
```

```text
SessionPresentation
├── record: SessionRecord metadata + hydrated history window
├── execution / availability / attention flags
├── runtime_attachment: Detached | Attaching | Attached
├── draft / pending_submission
└── scroll_anchor
```

`runtime_attachments` 只保存当前进程持有的临时 handle/lease；持久化层只记录 Runtime interruption/recovery facts，绝不序列化 handle、Future、socket 或进程内 `SessionRuntime`。

不需要立即引入通用组件框架，但至少需要：

- 正交的 App health states；
- `SessionRecord`/`SessionStore` 与 `RuntimeAttachment` 的清晰边界；
- `OverlayFrame`、`OverlayStack` 与 `FocusToken`；
- `CommandRegistry`；
- `SettingsState`；
- `ModelPickerState`；
- `NotificationState`；
- `ConfigApplyState`（Desired/Effective/LKG + revision）；
- 用于 auth、runtime 和 config 的 stale-event rejection。

## 19. 前端验收清单

- [ ] 零配置执行 `bone` 会先看到 TUI，而不是 Shell 错误。
- [ ] 配置/数据目录不存在时由后台自动创建，不要求用户寻找文件，也不向 Workspace 写 `.bone/`。
- [ ] 未配置、未登录、无模型时仍显示工作台、Composer、历史和设置入口。
- [ ] 欢迎文案只描述 capability registry 中实际存在的文件操作，不对只读版本承诺修改文件。
- [ ] 首次启动只需一个主要动作即可开始认证。
- [ ] 用户可以在认证前写草稿，并在取消后完整恢复。
- [ ] SessionRecord 可在无 AgentHost 时创建；浏览历史不附着 Runtime，提交 durable 消息后才惰性附着。
- [ ] Session 的 lifecycle、execution、runtime attachment、availability 和 attention flag 可组合展示。
- [ ] `/` 在逐键输入过程中实时打开命令候选，`//text` 会把 `/text` 发送给模型。
- [ ] 单行或多行粘贴都不能未经独立确认执行本地命令；不支持 bracketed paste 时启用保守模式。
- [ ] 未知 slash command 不发送给模型。
- [ ] `/model <id>`、`default`、`global`、`inherit` 和 `search` 的解析、scope 与错误行为符合 7.2。
- [ ] `/config` 不显示 JSON，并覆盖 capability registry 中所有正式公开设置。
- [ ] 每个字段只能选择 scope 矩阵允许的范围；`R` 恢复继承，安全子 scope 不能扩大父级权限。
- [ ] 设置无需总保存按钮、reload 或重启。
- [ ] 每个变更可区分 Candidate、Validating、Preparing、Saved、Pending、Applied 与两类 Failed。
- [ ] 只有 consumer ack 的 effective revision 显示“已应用”；文件写入或 UI 更新不能冒充 runtime 生效。
- [ ] apply 失败后 LKG 与旧 runtime 一致可用，并准确区分“未保存”和“已保存但未应用”。
- [ ] `/model` 默认只影响当前对话。
- [ ] 模型目录项标注 authoritative、缓存或未验证候选来源，普通用户不需要记忆 ID。
- [ ] 运行中切模型明确显示当前 User Turn 保持原 TurnConfig，下一条用户消息生效。
- [ ] Workspace 设置说明影响了多少继承 Session。
- [ ] 新连接准备好之前旧连接继续；失败时新配置不替换 Effective。
- [ ] 登录页只在 opener 成功后称为“已打开”，只在服务返回 expiry 时倒计时。
- [ ] 取消登录会撤销 attempt、清除 code、恢复焦点和草稿，迟到成功不会提交凭据或消息。
- [ ] Settings → Model Picker → Confirmation 可以嵌套，只有栈顶接收输入且各层状态保留。
- [ ] `Esc` 每次只 pop 一个 Overlay 并恢复打开前焦点，不停止后台任务。
- [ ] 背景 Session 完成不抢焦点。
- [ ] Session rail 只展示当前 Workspace。
- [ ] 40 列窄屏可用 Header Enter 和 `/sessions` 进入 Session 列表，不依赖 `Ctrl-Left`。
- [ ] 同一 Workspace 重启会恢复上次 Session、历史和草稿，但不会自动重跑 Interrupted 任务或未知工具效果。
- [ ] 同一 Session 被其他进程持有时只读打开或受控接管，不能双写。
- [ ] 跨 Workspace Session 不能被恢复、重绑、Fork 或复制到当前目录，只提供回原目录打开的命令。
- [ ] 配置损坏时用户可自动修复、临时继续或看诊断。
- [ ] SessionStore 不可写时发送被禁用，内存草稿和可能丢失的内容有明确标记。
- [ ] one-shot CLI 不进入 TUI 或设备登录，复用 Workspace/config/SessionStore，`--model` 只生成 Session override。
- [ ] one-shot 创建的 Session 后续出现在同 Workspace TUI；非 TTY 错误无 ANSI、secret 或虚假交互提示。
- [ ] 40、80、120 列终端都存在可操作布局。
- [ ] 状态不只依赖颜色，所有主流程可用方向键/Tab/Enter 完成，单字母快捷键不劫持文本输入。
- [ ] 异常矩阵中每个异步状态都有成功、失败、取消或可操作等待终点，且关闭详情能恢复原焦点。

## 20. 设计评审需确认的决策

以下决策不改变本稿已经固定的用户契约，但需要工程或产品在对应阶段选型：

1. `/doctor` 是否保留为 `/config doctor` 的公开别名；无论别名与否，后者必须可用；
2. 哪些终端可靠传递 `Ctrl-,`、`Ctrl-Left/Right` 与 bracketed paste；探测失败时必须使用本文 slash/Enter fallback；
3. 多进程 credential 协调采用常驻 broker 还是短时 refresh lock；用户侧仍必须支持多个 Workspace 并行且不能泄露 secret；
4. Session archive 的默认保留周期、可恢复删除窗口和存储配额；
5. 当前 Provider 能否提供 authoritative model listing；不能时使用带“尚未验证”标签的版本化 catalog 与提交前能力检查；
6. 引入写工具前，首次 Workspace 信任确认的内容和触发时机；当前只读能力不显示虚假写入承诺；
7. 写工具上线时采用 Workspace 单写者、文件 revision 冲突检测、可选 worktree，或三者组合；
8. 支持“请求接管”所需的跨进程通知机制；不能安全实现的平台保持只读。

已在本文固定、不得再由实现自行选择的行为包括：恢复上次选中 Session、一个 User Turn 锁定 TurnConfig、`//` 转义、粘贴保守策略、Overlay stack 返回焦点，以及 vNext 禁止跨 Workspace Fork/rebind。

## 21. 原型数据说明

交互原型中的模型名、账号和任务内容仅用于展示布局与状态，不是硬编码产品数据。正式实现必须从认证状态、ModelCatalog、SessionStore 和 capability registry 中读取。
