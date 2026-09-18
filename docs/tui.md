# BONE TUI 产品与实现契约

状态：主界面已重写，模型、回答、恢复与详情入口已接入；终端平台、输入、编辑器和 UI 基础已在单一 `bone-tui` crate 内拆分。Linux PTY 自动化覆盖当前终端协议与恢复路径，macOS 和原生 Windows 的真机认证仍未完成。真实模型运行验收需要可用接入（API 凭据或账号授权）。本文记录当前实现契约；验收仍对照已批准的设计与交互规格，后续用户明确反馈优先。本文不能用于排除尚未实现或尚未验证的原始要求。

## 产品定位

`bone` 是 coding agent 的终端前端，不是 Dashboard、管理后台或功能目录。常态界面只让用户完成三件事：切换 Session、与 Agent 对话、在需要时深入查看一个对象。

视觉采用纯黑侧栏、炭黑主画布和中性灰编辑面。橙色只用于当前 Session 左侧的身份条，以及处于可见相位的应用内 caret；它不承担区域焦点、列表候选、运行状态或分隔条语义。层级来自留白、明度、中性表面和统一 label 字重，不复制其他产品的品牌视觉。

## 稳定三栏

```text
┌ Session rail ┬ Conversation + composer ┬ Extension pane ┐
│ 切换会话      │ 唯一主工作面             │ 当前对象的深度   │
└───────────────┴─────────────────────────┴────────────────┘
```

- 左栏固定负责 Session 切换。每个 Session 固定三行内容：标题、最近一条 Agent 回复预览、消息数与相对时间或日期；下一行是 Session 间分隔，不因草稿或运行状态改变行高。状态使用标题旁的紧凑语义点。当前 Session 始终以左侧橙色身份条标识；只有 Sessions 区域拥有焦点时，键盘候选才使用浅中性的 `SELECTED` 背景。摘要投影尚未追上历史时，回复行显示 `Loading history…`，计数显示 `… msgs`，不把部分值伪装成完整事实。
- 中栏固定负责对话。可直接编辑的 Session 标题、历史、临时活动和 Composer 都在这里；Composer 绝不横跨三栏。
- 右栏默认显示中性详情表面；打开 Job 或工具结果详情时用于阅读当前对象，Esc 返回来源。窄屏详情使用中栏。材料、架构图、Git 等尚未实现，不放占位功能。
- 不存在全局顶栏、动作栏、Dashboard 首页、设置页、详情 tab 或创建 Session 对话框。

响应式规则：`>=140` 列显示三栏（默认左 32、右 40、中间取余，可拖动分隔条调整）；`100–139` 列显示左中两栏；`40–99` 列显示当前单区；小于 40 列或高度小于 12 行显示过小提示并保留草稿和退出能力。

## 对话视觉

- 用户消息只用低对比背景和上下留白形成独立组，不显示 `YOU` 前缀，也不增加细竖线或说话者标签。
- Agent 回复直接生长在主画布上，不显示 `BONE` 前缀，也不套消息卡片。
- 工具、问题、状态和活动使用紧凑行；Runtime started/reconfigured/closed 等传输生命周期不进入对话。
- 长回复必须完整可达，按真实终端视觉行滚动。渲染产生的换行 metrics 同时驱动键盘/鼠标滚动、历史预取和缓存淘汰补偿，不能用事件数量猜视觉行数。
- 外部文本在渲染前过滤 ANSI、OSC、C0/C1 与双向控制字符。中文、组合字符和 ZWJ emoji 的截断、换行与光标按 grapheme 和显示宽度处理。

## 输入与命令

键盘是第一公民，鼠标是同等正式入口，但不要求用户背一张快捷键表。

- Workspace 有且只有四个焦点区域：`Sessions`、`SessionTitle`、`Composer`、`RightRail`。`Ctrl+←/→` 在侧栏与上次使用的中栏编辑区之间移动，`Ctrl+↑/↓` 在 `SessionTitle` 与 `Composer` 之间移动；到达边界不循环，窄屏未显示右栏时也不会把焦点移入隐藏区域。Conversation transcript 是可滚动内容，不是第五个焦点区域。详情和普通面板打开时独占焦点作用域，关闭后恢复准确的 workspace 焦点；附着于 Composer 的 `/` 命令面板保留 Composer 焦点。
- Session 列表用方向键浏览候选，Enter 打开，点击直接打开；滚轮只滚列表，Esc 返回当前 Session。切换 Session 保留切换前的四区域焦点，`SessionTitle` 焦点会转到新 Session 的内联标题编辑器。`SessionTitle` 与 `Composer` 聚焦时均可用 PageUp/PageDown 阅读 transcript；鼠标滚轮也可直接滚动 transcript。
- Composer 正文上下各留一行，模型与快捷键放在框外 status baseline；按视觉行增长，小窗口优先保留至少三行历史。`SessionTitle` 和 Composer 都只用 caret 表达编辑焦点，不绘制焦点 gutter、橙色短轨或 active 标题色。caret 每 500 ms 切换一次可见相位，完整闪烁周期为 1 秒；输入、焦点变化和 resize 会从可见相位重新开始。可见时，BONE 绘制 `FOCUS_MARK` Cell，并把原生 cursor 定位到同一 Cell 供 IME 与辅助功能使用。
- 上下方向键按视觉行移动输入光标，连续移动保留首选列；软换行、中文、组合字符和 CRLF 共用文本几何。
- Shift+方向/Home/End 选择文字，鼠标点击定位及拖选；Alt+左右按 Unicode 词边界移动，Ctrl+Z/Y 撤销重做。普通、问题回答和未建会话草稿各自保留编辑历史；撤销也递增 revision，旧提交回执不能清空撤回后的文字。
- `Enter` 发送；`Shift+Enter` 换行。Unix/WSL 启动时查询键盘增强能力，只在确认支持后于本次运行期间 push CSI-u/Kitty 模式；原生 Windows 使用带修饰键的控制台事件。能力不足时，status baseline 将换行提示标为 unavailable，帮助面板显示原因；不增加 `Alt+Enter`、`Ctrl+J` 或其他隐藏别名。
- `Esc` 先关闭菜单或详情；没有菜单时只在工作确实运行时停止，不把普通返回误作停止。
- `Ctrl+C` 清空当前输入框，可用 `Ctrl+Z` 撤销；空输入不退出、不停止工作。只在输入框拥有焦点且没有弹层时生效，问题回答使用独立缓冲。
- `Ctrl+D` 是唯一退出快捷键，安全退出并保留普通未提交草稿。
- 粘贴只插入文本，不直接执行命令。

全局动作统一从 Composer 的 `/` 命令面板进入，没有 `Ctrl+P` 快捷键。面板随命令前缀过滤，保持 Composer 的输入焦点和 caret；底部模型入口也可点击。已实现：

- `/new [title]`：直接创建 Session，无确认框；省略标题时，App 在首次提交后生成标题。
- `/sessions`：聚焦 Session rail。
- `/rename`：进入当前 Session 的单行内联标题编辑；`/rename <title>` 直接写入并提交标题。
- `/model`：唯一的模型入口，不接受隐藏的 profile/model 参数。第一屏是连接 tab 条：每个已保存连接一个 tab，末尾固定是 `+ Add connection`，没有保存过的连接类型不占 tab。左右方向键切 tab，上下方向键在当前 tab 内选模型，Enter 应用；每个 tab 的末行是手工输入 model ID，从 provider 文档复制来的 ID 直接粘贴即可。tab 条上按 `e` 编辑当前连接（ChatGPT 连接直接重新走登录），按 `d` 删除当前连接并先问 y / n 确认，`Delete` 只从当前连接移除一个已保存的模型——目录里出现但连接没保存过的模型没有可移除的对象。新建流程从 `+ Add connection` 开始：六行 kind picker（ChatGPT 订阅、OpenAI API、Anthropic API，以及 OpenAI-compatible Responses / Chat Completions 和 Anthropic-compatible Messages），选完再填表——官方表单只有 Key 与 Model，compatible 类型才需要 Name 与 URL；已经保存的类型直接跳到它的 tab，不会第二次索要凭据。选择 ChatGPT 订阅或 OpenAI Responses 的模型后，下一步明确选择 `provider default`、`minimal`、`low`、`medium`、`high`、`xhigh` 或 `max`；其他协议直接应用，不隐藏默认思考深度。ChatGPT 账号只在用户主动新建该连接时才出现。列表只显示用户保存的连接和模型，以及当前仍在使用的选择；没有隐式 ChatGPT、官方预设、推荐文案或模型营销介绍。删除连接会先清凭据再从 `config.toml` 移除；仍引用它的 Session 保留原选择并显示 `Model setup needs attention`，重新选一个模型即可修复。选中 Session 时的切换同时更新该 Session 的 worker/coordinator；未选中 Session 时更新 workspace，首次缺模型时同一选择也会成为 workspace 默认。执行中可以切换模型，密钥与设备码保持脱敏；不再提供独立 `/login`、`/connect` 或旧 `/model <profile> <model>` 语法。
- `/answer`：回答当前仍有效的问题，使用独立缓冲和完整 QuestionId；过期答案只能明确转为普通草稿，不会自动当新请求发送。
- `/details`：列出当前任务及已加载历史中的工具/任务结果，选择后读取完整对象；鼠标也可从可见对象行进入。
- `/recover`：恢复最近可恢复输入的原文。
- `/retry`：优先重试原提交身份，或重试已保存输入。
- `/help`：显示命令提示。
- `/quit`：清除命令文本、保存草稿并退出。

创建使用持久 `RequestId`。创建结果不确定时只能 `/new --retry` 复用同一身份；不能“取消后用新身份重建”，否则 App 已经持久化但前端未收到快照时会产生重复 Session。

## App-only 边界

TUI 只是前端。所有产品事实和操作都经过 `bone-app`：

```text
keyboard / mouse / resize
           ↓
        UI event
           ↓
       reducer ─────→ render
           ↓
      typed effect
           ↓
     bone-app public API
           ↓
 result / observe / history → UI event
```

TUI 只拥有终端生命周期、当前焦点、选择、每 Session 的草稿编辑缓冲、阅读位置和有界渲染缓存。它不得直接读取 SQLite、项目文件、Git、配置文件、凭据或 provider，不得依赖 Core/Adapter，也不得从日志文字推断完成、验收或证据。

当前为 TUI 补齐的 App 契约包括：幂等 Session 创建、最后活动 Session、workspace 配置解析、首次输入自动标题，以及供 Session rail 使用的 durable `SessionSummary`。摘要包含创建时间、消息计数、最近 Agent 回复的有界预览和 `projection_pending`；App 以持久 cursor 增量推进投影，每次 overview 只处理有界数量与字节量，TUI 不扫描每个 Session 的完整历史。Draft / Needs you / Can resume 等状态也来自 App 的只读 overview；浏览列表不取得每个 Session 的 writer lease。

## 正确性与性能

- 视图刷新以 Session 和 generation 限制归属；提交回执以 Session 和 RequestId 核销，允许提交期间切走再切回。旧回执不能清掉新编辑。
- 草稿按 revision 保存；提交回执只清除对应版本，用户随后输入的文字不得丢失。
- history 是持久事实，watch 是可合并快照。历史分页按 sequence 去重并保持 cursor 单调。
- `SessionSummary` 是 history 的 durable、bounded、incremental projection。新写入在投影已追平时与 history append 同事务推进；旧数据或落后投影由后续 overview 从持久 cursor 继续，每个 workspace overview 最多推进 64 条记录和 8 MiB payload。`projection_pending` 表示计数与预览仍只覆盖已投影前缀，Session rail 必须显示 pending，而不能把它们标成最终值。
- 阅读旧内容时，新尾部不会抢焦点；满缓存向前补读时按真实视觉行补偿阅读锚点。
- 共享缓存预算按用途分配：所有 Session 历史共 16 MiB，全部编辑撤销历史共 8 MiB，当前详情排版及投影缓存准入 8 MiB；当前草稿不参与淘汰。结构化结果使用紧凑无损 JSON；任务关联输入只借用当前 snapshot 的可见 ID 切片，完整列表可滚动读取，不拼接整表。超预算排版不准入缓存；不能据此宣称进程 RSS 始终不超过 32 MiB。
- 仅 dirty 时绘制；每次状态变脏后，在接收下一个输入或运行时事件前立即提交新帧。没有常驻帧率 ticker；只有编辑焦点活跃时运行 500 ms caret 相位计时，组成 1 秒完整闪烁周期。后台草稿与 overview 计时本身不强制重绘。渲染路径不做产品 I/O。
- raw mode、alternate screen、鼠标捕获、Unix bracketed paste、键盘增强和光标可见性由 `TerminalSession` 与模式账本统一管理。唯一输入 worker 在同一线程调用 Crossterm `poll/read`；每次恢复先请求停止并等待该线程 `join`，再按逆序 best-effort 释放模式。后台 panic hook 只通知 runner 并等待独立恢复屏障，终端恢复后才调用既有 hook；runner 或输入线程自身 panic 则在 unwind 恢复后输出有限简报。可捕获的退出信号先恢复终端再进入有界 shutdown；Unix suspend 前恢复，continue 后重新进入、重新协商键盘能力并创建新 reader。Crossterm 0.28 的原生 Windows reader 不会产生 `Event::Paste`，因此该路径不启用 bracketed paste，也不宣称能区分粘贴换行与物理 `Enter`；既定的 `Enter` 提交、`Shift+Enter` 换行契约保持不变，原生 Windows 的安全多行粘贴仍是发布前待解决的输入 transport 限制。
- BONE 不写宿主配置，也不设置宿主字体、字号、cursor 颜色、cursor 形状、terminal title 或 palette。这些属性在应用退出后不应因 BONE 留下变化。

详细自动化、PTY、性能与平台门禁见 [tui-quality-gates.md](tui-quality-gates.md)。

## 当前明确边界

已接入真实 Session、配置、历史、登录、问题回答、任务/工具详情与恢复操作。材料、Git 等未实现。普通草稿通过 App 持久保存；尚无 Session 的非空草稿退出时会幂等创建会话再保存。问题专用缓冲在进程内按 Session/Question 保留，退出时把未发送文字加上明确标记追加到普通草稿，重启后可审阅，不恢复过期问题绑定，也不自动发送。保存失败时保持界面与文字，重复退出复用创建身份。真实模型执行、流式回复、停止与问题回答的人工验收目前仍需独立登录，自动化测试不能替代这项验收。

最新宽度调整：左栏默认 32 格；对话与 Composer 共用中栏左右各 4 格边距，不再限制 84 格最大宽度，随中栏一同拉伸。

本轮视觉细化（2026-09-11）：分栏使用 #282828 的连续整格背景，拖动时临时提升为 #707070；侧栏与空详情区为 #090909，主画布为 #121212。正文与次要文字分级，输入正文、模型和命令提示内缩两格，与会话标题及回复正文对齐。详见 design/tui-redesign/quiet-layout/REVIEW.md。

组件尺寸更新（2026-09-11）：Session 条目固定为标题、最近回复、消息数与时间三行，条目之间另有一行分隔。终端高度至少 24 行时，输入正文最少两行，菜单和接入字段使用两行步长；低于 24 行保持紧凑布局。用户消息、回复、标题与输入统一文字左边缘。终端字体与字号由宿主终端控制，BONE 不修改。详见 design/tui-redesign/comfortable-layout/REVIEW.md。

跨平台可读性：正文 #eeeeee，次要文字 #aeaeae；普通会话标题不再降为次要文字亮度。所有正文和辅助信息使用宿主终端的同一 Cell 字号，不制造“小字”层；只有短结构标签使用统一 label 粗度，正文不使用 DIM。字体像素仍由宿主终端负责；BONE 通过 Cell 密度、显式颜色和语义层级保持一致，不把修改宿主字体作为前提。

连续栏界与字重更新：左右栏界改为一列背景色单元格，覆盖完整行高，不依赖 `│` 字形拼接。正文保持 regular；区域标题、面板标题、输入提示和快捷键 chord 使用同一个语义 label 粗度，并同时依赖亮度、背景或间距形成层级。输入正文默认两行，下面只有一行 status baseline。

栏宽拖动：按住左右分隔条并拖动，调整对应侧栏；拖动中使用明亮中性结构色，松开结束。左右侧栏各至少 24 列，对话区至少 56 列（含内边距）。窗口缩小时临时约束显示宽度，放大后恢复用户偏好；切换会话或面板保留栏宽。宽度目前保存在本次运行的 UI 状态中，重启恢复默认。Esc 或窗口尺寸变化结束拖动；拖动经过编辑区不修改草稿。面板打开时分隔条保持可见但不接收鼠标。两栏模式只提供左分隔条，单栏不显示拖动入口。

输入区细化：Composer 与内联 Session 标题编辑器只在可见相位绘制应用 caret，没有焦点 gutter 或橙色短轨；用户消息不复用焦点色。模型及运行/保存状态位于输入框外的统一 status baseline；Shift+Enter、Ctrl+C、Ctrl+D 的可见提示直接来自正式 keymap，并按可用宽度省略；发送提示保持右对齐。输入 `/` 打开的命令面板与其他面板使用同一套中性表面、标题、分隔和选中样式，同时仍附着于 Composer 并保留 caret。切换 Session 会保留 `Sessions`、`SessionTitle`、`Composer` 或 `RightRail` 中当前所在的区域。

焦点表达统一：`Sessions` 通过浅中性的 `SELECTED` 候选背景表达键盘位置，当前 Session 另用始终可见的左侧橙色身份条；`SessionTitle` 与 `Composer` 只通过 1 秒周期的 caret 表达焦点；`RightRail` 与独占面板使用中性背景层级。Conversation 没有独立 focus，任何区域都不使用焦点 gutter 或橙色短轨。橙色只可能出现在当前 Session 身份条和当前可见 caret 上。
