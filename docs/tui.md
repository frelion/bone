# BONE TUI 产品与实现契约

状态：对话外壳已重写。本文是当前 TUI 的唯一有效约定；旧设计稿只作为历史材料，不能覆盖这里已经确定的布局、输入与 App 边界。

## 产品定位

`bone` 是 coding agent 的终端前端，不是 Dashboard、管理后台或功能目录。常态界面只让用户完成三件事：切换 Session、与 Agent 对话、在需要时深入查看一个对象。

视觉气质面向偏理工、开放但内向的用户：安静、克制、信息密度高，接近 OpenCode 的终端原生感。层级主要依靠留白、明度、细分隔线和极少量强调色，不依靠大卡片、身份标签或装饰性标题。

## 稳定三栏

```text
┌ Session rail ┬ Conversation + composer ┬ Extension pane ┐
│ 切换会话      │ 唯一主工作面             │ 当前对象的深度   │
└───────────────┴─────────────────────────┴────────────────┘
```

- 左栏固定负责 Session 切换。每项只显示标题和一个短状态；状态来自 App 的 workspace overview，本地 live snapshot 只补充实时运行状态。
- 中栏固定负责对话。标题、历史、临时活动和输入框都在这里；Composer 绝不横跨三栏。
- 右栏是扩展面板，将来用于 Job、材料、架构图、Git、证据等当前对象。本阶段故意保持空白、无焦点、无点击目标，不放占位功能。
- 不存在全局顶栏、动作栏、Dashboard 首页、设置页、详情 tab 或创建 Session 对话框。

响应式规则：`>=140` 列显示三栏（左 24、右 40、中间取余）；`100–139` 列显示左中两栏；`40–99` 列显示当前单区；小于 40 列或高度小于 12 行显示过小提示并保留草稿和退出能力。

## 对话视觉

- 用户消息用低对比背景加一条细竖线表达来源，不显示 `YOU` 前缀。
- Agent 回复直接生长在主画布上，不显示 `BONE` 前缀，也不套消息卡片。
- 工具、问题、状态和活动使用紧凑行；Runtime started/reconfigured/closed 等传输生命周期不进入对话。
- 长回复必须完整可达，按真实终端视觉行滚动。渲染产生的换行 metrics 同时驱动键盘/鼠标滚动、历史预取和缓存淘汰补偿，不能用事件数量猜视觉行数。
- 外部文本在渲染前过滤 ANSI、OSC、C0/C1 与双向控制字符。中文、组合字符和 ZWJ emoji 的截断、换行与光标按 grapheme 和显示宽度处理。

## 输入与命令

键盘是第一公民，鼠标是同等正式入口，但不要求用户背一张快捷键表。

- `Ctrl+←/→/↑/↓` 在左栏、对话与 Composer 之间做空间焦点移动；右栏尚未启用焦点。
- Session 列表用方向键选择；对话区用方向键或 PageUp/PageDown 阅读。
- `Enter` 发送；`Shift+Enter` 或 `Alt+Enter` 换行。
- `Esc` 先关闭 slash palette；没有 palette 时只在工作确实运行时停止，不把普通返回误作停止。
- `Ctrl+Q` / `Ctrl+C` 安全退出并保留普通未提交草稿。
- 粘贴只插入文本，不直接执行命令。

设置、创建和其他全局动作统一从 Composer 的 slash command 进入，不新增页面级入口。本阶段已实现：

- `/new [title]`：直接创建 Session，无确认框；省略标题时，App 在首次提交后生成标题。
- `/sessions`：聚焦 Session rail。
- `/rename <title>`：修改当前 Session 标题。
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

当前为 TUI 补齐的 App 契约包括：幂等 Session 创建、最后活动 Session、workspace 配置解析、首次输入自动标题。Session rail 的 Draft / Needs you / Can resume 等状态也来自 App 的只读 overview；浏览列表不取得每个 Session 的 writer lease。

## 正确性与性能

- 每个异步结果携带 Session、generation 和请求身份；迟到响应不能覆盖新选择。
- 草稿按 revision 保存；提交回执只清除对应版本，用户随后输入的文字不得丢失。
- history 是持久事实，watch 是可合并快照。历史分页按 sequence 去重并保持 cursor 单调。
- 阅读旧内容时，新尾部不会抢焦点；满缓存向前补读时按真实视觉行补偿阅读锚点。
- TUI 历史总预算为 32 MiB，不是每 Session 32 MiB；草稿不参与淘汰。
- 仅 dirty 时绘制，更新合并到约 30 FPS；渲染路径不做产品 I/O。
- raw mode、alternate screen、鼠标捕获、bracketed paste 和光标由 RAII guard 恢复；正常退出、信号与有界 shutdown 都必须先恢复终端。

详细自动化、PTY、性能与平台门禁见 [tui-quality-gates.md](tui-quality-gates.md)。

## 当前明确边界

这次完成的是干净的三栏对话外壳、真实 Session/配置/历史数据流和可靠输入闭环。右侧扩展内容以及更多设置、登录、Job、材料、Git 和验收命令仍按同一结构继续设计和接线；它们不能以旧页面、假数据、无行为按钮或绕过 App 的方式回填。
