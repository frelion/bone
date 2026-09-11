# BONE TUI 产品与实现契约

状态：主界面已重写，模型、回答、恢复与详情入口已接入，真实模型运行验收需要可用接入（API 凭据或账号授权）。本文记录当前实现契约；验收仍对照已批准的设计与交互规格，后续用户明确反馈优先。本文不能用于排除尚未实现或尚未验证的原始要求。

## 产品定位

`bone` 是 coding agent 的终端前端，不是 Dashboard、管理后台或功能目录。常态界面只让用户完成三件事：切换 Session、与 Agent 对话、在需要时深入查看一个对象。

视觉采用纯黑侧栏、炭黑主画布、中性灰编辑面和鲜橘黄强调色。代码和结果使用青、紫、绿辅助表达语义；层级来自留白、明度和明确的状态，不复制其他产品的品牌视觉。

## 稳定三栏

```text
┌ Session rail ┬ Conversation + composer ┬ Extension pane ┐
│ 切换会话      │ 唯一主工作面             │ 当前对象的深度   │
└───────────────┴─────────────────────────┴────────────────┘
```

- 左栏固定负责 Session 切换。普通会话只显示标题，草稿加小标记；需要处理或正在工作的会话增加一行短状态，不重复显示“就绪”；状态来自 App 的 workspace overview，本地 live snapshot 只补充实时运行状态。
- 中栏固定负责对话。标题、历史、临时活动和输入框都在这里；Composer 绝不横跨三栏。
- 右栏默认留空；打开 Job 或工具结果详情时用于阅读当前对象，Esc 返回来源。窄屏详情使用中栏。材料、架构图、Git 等尚未实现，不放占位功能。
- 不存在全局顶栏、动作栏、Dashboard 首页、设置页、详情 tab 或创建 Session 对话框。

响应式规则：`>=140` 列显示三栏（左 32、右 40、中间取余）；`100–139` 列显示左中两栏；`40–99` 列显示当前单区；小于 40 列或高度小于 12 行显示过小提示并保留草稿和退出能力。

## 对话视觉

- 用户消息用低对比背景加一条细竖线表达来源，不显示 `YOU` 前缀。
- Agent 回复直接生长在主画布上，不显示 `BONE` 前缀，也不套消息卡片。
- 工具、问题、状态和活动使用紧凑行；Runtime started/reconfigured/closed 等传输生命周期不进入对话。
- 长回复必须完整可达，按真实终端视觉行滚动。渲染产生的换行 metrics 同时驱动键盘/鼠标滚动、历史预取和缓存淘汰补偿，不能用事件数量猜视觉行数。
- 外部文本在渲染前过滤 ANSI、OSC、C0/C1 与双向控制字符。中文、组合字符和 ZWJ emoji 的截断、换行与光标按 grapheme 和显示宽度处理。

## 输入与命令

键盘是第一公民，鼠标是同等正式入口，但不要求用户背一张快捷键表。

- `Ctrl+←/→/↑/↓` 在左栏、对话与 Composer 之间做空间焦点移动；详情打开时单独处理滚动和返回。
- Session 列表用方向键浏览候选，Enter 打开，点击直接打开；滚轮只滚列表，Esc 返回原对话。对话区用方向键或 PageUp/PageDown 阅读。
- Composer 正文上下各留一行，模型与快捷键放在框外底行；按视觉行增长，最多六行，小窗口优先保留至少三行历史。光标为终端实心块，离开输入或打开菜单后隐藏。
- 上下方向键按视觉行移动输入光标，连续移动保留首选列；软换行、中文、组合字符和 CRLF 共用文本几何。
- Shift+方向/Home/End 选择文字，鼠标点击定位及拖选；Alt+左右按 Unicode 词边界移动，Ctrl+Z/Y 撤销重做。普通、问题回答和未建会话草稿各自保留编辑历史；撤销也递增 revision，旧提交回执不能清空撤回后的文字。
- `Enter` 发送；`Shift+Enter` 或 `Alt+Enter` 换行。
- `Esc` 先关闭菜单或详情；没有菜单时只在工作确实运行时停止，不把普通返回误作停止。
- `Ctrl+Q` / `Ctrl+C` 安全退出并保留普通未提交草稿。
- 粘贴只插入文本，不直接执行命令。

全局动作可从 slash command 或 `Ctrl+P` 命令菜单进入。命令菜单不占用普通草稿；左栏新建和底部模型、命令文字也可点击。已实现：

- `/new [title]`：直接创建 Session，无确认框；省略标题时，App 在首次提交后生成标题。
- `/sessions`：聚焦 Session rail。
- `/rename <title>`：修改当前 Session 标题。
- `/model`：统一模型选择、添加/编辑接入、API 密钥与账号授权。模型列表中选择已有配置、编辑接入或添加接入；表单支持 Tab/上下切字段、Ctrl+U 清空和鼠标操作。API 支持 Responses、Chat Completions、Anthropic Messages 及自定义 HTTPS 地址；密钥遮罩、退出清空、不进草稿和 Debug。账号授权也在此流程内返回模型列表，不再提供独立 `/login` 或 `/connect`。兼容 `/model <profile> <model>` 快捷参数；`/model <profile> <model>` 设置当前 Session（尚无 Session 时设置 workspace）的 Worker。列表不是服务端模型目录。运行模型与保存配置分别显示，比较完整 profile/model/options；保存成功不等同运行已应用。
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

当前为 TUI 补齐的 App 契约包括：幂等 Session 创建、最后活动 Session、workspace 配置解析、首次输入自动标题。Session rail 的 Draft / Needs you / Can resume 等状态也来自 App 的只读 overview；浏览列表不取得每个 Session 的 writer lease。

## 正确性与性能

- 视图刷新以 Session 和 generation 限制归属；提交回执以 Session 和 RequestId 核销，允许提交期间切走再切回。旧回执不能清掉新编辑。
- 草稿按 revision 保存；提交回执只清除对应版本，用户随后输入的文字不得丢失。
- history 是持久事实，watch 是可合并快照。历史分页按 sequence 去重并保持 cursor 单调。
- 阅读旧内容时，新尾部不会抢焦点；满缓存向前补读时按真实视觉行补偿阅读锚点。
- 共享缓存预算按用途分配：所有 Session 历史共 16 MiB，全部编辑撤销历史共 8 MiB，当前详情排版及投影缓存准入 8 MiB；当前草稿不参与淘汰。结构化结果使用紧凑无损 JSON；任务关联输入只借用当前 snapshot 的可见 ID 切片，完整列表可滚动读取，不拼接整表。超预算排版不准入缓存；不能据此宣称进程 RSS 始终不超过 32 MiB。
- 仅 dirty 时绘制，更新合并到约 30 FPS；渲染路径不做产品 I/O。
- raw mode、alternate screen、鼠标捕获、bracketed paste 和光标由 RAII guard 恢复；正常退出、信号与有界 shutdown 都必须先恢复终端。

详细自动化、PTY、性能与平台门禁见 [tui-quality-gates.md](tui-quality-gates.md)。

## 当前明确边界

已接入真实 Session、配置、历史、登录、问题回答、任务/工具详情与恢复操作。材料、Git 等未实现。普通草稿通过 App 持久保存；尚无 Session 的非空草稿退出时会幂等创建会话再保存。问题专用缓冲在进程内按 Session/Question 保留，退出时把未发送文字加上明确标记追加到普通草稿，重启后可审阅，不恢复过期问题绑定，也不自动发送。保存失败时保持界面与文字，重复退出复用创建身份。真实模型执行、流式回复、停止与问题回答的人工验收目前仍需独立登录，自动化测试不能替代这项验收。

最新宽度调整：左栏固定 32 格；对话与 Composer 共用中栏左右各 4 格边距，不再限制 84 格最大宽度，随中栏一同拉伸。
