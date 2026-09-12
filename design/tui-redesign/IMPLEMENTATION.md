# BONE TUI 实现记录

本轮将已确认设计接入生产 Rust TUI，继续补齐模型、登录、回答、恢复与详情操作。所有产品操作仍经 bone-app。整体交付 goal 仍在进行，真实模型验收尚未完成。

## 已实现

- 纯黑侧栏、炭黑主画布、中性灰编辑面、橘黄色选择线与动作；代码和结果使用语义颜色。
- 左栏保留项目、会话标题、草稿标记、需要关注或活动状态。普通会话一行，不重复显示 Ready。
- 中栏使用局部编辑面，正文上下各一行留白，模型与快捷键放在框外。按视觉行增长、最多六行，最小窗口保留至少三行正文。
- 160/120/80/40 列布局；右栏保持空白。过小窗口仅保留退出与 resize，不允许隐藏的提交操作。
- 实心终端光标；输入与命令菜单只有一个可见焦点。实际光标颜色由终端主题决定。
- 上下移动按视觉行，连续移动保留首选列；CRLF、中文、组合字符、行尾插入位置共用文本几何。
- 菜单可见窗口跟随选择，点击与显示使用相同索引；发送区域共用文字矩形；空白右栏滚轮无动作。
- 提交回执按 Session + RequestId 核销，切走再切回不会永久卡住，revision 继续保护新编辑。
- 工作状态根据活动、输入与 Job，而非长驻 Runtime；不同 AppProblem 不再一律描述为配置错误。

## 代码结构

保留 event → reducer → typed effect → bone-app 的数据流。布局负责区域和命中，视图负责显示，text.rs 统一纯文本的显示宽度和编辑光标计算。没有新增产品依赖、通用组件框架、编辑器框架或缓存层。

## 验证入口

- `cargo test -p bone-tui --all-targets --all-features --locked`
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- `cargo test -p bone-tui --lib --release --locked tests::performance::release_tui_performance_harness -- --ignored --exact --nocapture`
- `BONE_TUI_PREVIEW_WIDTH=160 BONE_TUI_PREVIEW_HEIGHT=40 BONE_TUI_PREVIEW_SCENARIO=conversation BONE_TUI_PREVIEW_OUTPUT=preview.svg cargo test -p bone-tui --lib --locked tests::preview::render_preview_artifact -- --ignored --exact --nocapture`

crate 内私有的 preview 测试使用生产 renderer + Ratatui TestBackend 输出单元格，是确定性示例数据，不是模型真实运行记录。尺寸与输出文件均由显式环境变量指定；PTY 测试另行启动真实 binary，检查键盘与终端恢复。

## 范围

已接入模型选择与显式配置、独立登录、问题专用回答、过期答案保护、原请求重试、取消原文恢复、任务/工具详情及返回。首次普通输入会创建会话并提交。问题回答缓冲在进程内保持问题身份，退出时将未发送文字带标记追加到普通草稿。普通及尚无会话的草稿都通过 App 保存。Linux/WSL 手工验收和其他平台 CI 仍需对应环境。

## 初次外壳重写的历史结果（macOS；不代表后续功能复验）

- TUI 全目标：54 项通过；常规运行跳过的 release 性能测试已单独执行通过。
- 真实 binary PTY：3 项通过，包含正常退出和 Unix 信号退出的终端恢复。
- 全 workspace Clippy（warnings as errors）、fmt、doctests、rustdoc（warnings as errors）通过。
- Release：120×40 render p95 3.67 ms；160×50 render p95 3.36 ms；input-to-render p95 3.23 ms；缓存约 3.84 MB。RSS 在本机 harness 中不可用。
- 全 workspace 测试首次在 App 的 `a_detached_write_keeps_the_session_lease_until_execution_finishes` 出现持久提交未完成的时序失败；使用相同测试 binary 和独立 cargo 命令分别复跑均通过。未把首次全仓运行记为通过，也未修改 App 实现。
- 两位独立审查者复验：输入高度、CRLF、视觉行与首选列、提交回执、菜单窗口、TooSmall 隐藏操作、命中几何、活动与故障语义均无遗留阻断项。

[生产渲染器主图](b-complete/boards/implemented.svg) · [120列](b-complete/boards/implemented-120.svg) · [80列](b-complete/boards/implemented-80.svg) · [40列](b-complete/boards/implemented-40.svg)


## 后续实际使用与宽度修复（2026-09-10）

- 左栏从 24 扩到 32 个终端单元格。
- 去掉中栏内容 84 格上限。消息背景与输入背景共用 x/width，中栏左右各 4 格边距，随窗口一起变化。
- 启动实际 `target/debug/bone`，使用独立 `/tmp/bone-product-verify-20260910` 工作区和数据库，通过 PTY 输入真实按键并读取 ANSI。
- 已实际走过：创建会话、中文/组合字符多行粘贴、缩放到 120×30 与 40×12、退出恢复终端、重启恢复草稿、提交并持久保存、配置 Worker、打开命令菜单、键盘选择模型菜单和返回。
- 配置后真实 App 返回 Needs login。已请求用户完成 BONE 独立 ChatGPT 登录；尚未声称验证真实回复、停止运行或实际模型发起问题。对应纯状态/渲染测试不能替代该项验收。
- 初始 CLI 继承 NO_COLOR=1，旧捕获因此没有颜色。后续专用验证 PTY 使用 TERM=xterm-256color / COLORTERM=truecolor 并移除 NO_COLOR；产品仍尊重用户终端环境。
- 图片是从真实 PTY ANSI 网格重建的浏览器预览，非操作系统终端截图，非示例数据。JSON 同时保存实际终端尺寸、光标位置及隐藏状态；光标外观不能由静态网格证明。

[实际消息/输入框对齐](product-verification/17-actual-message-composer-aligned.jpg) · [160 列真实网格](product-verification/17-actual-message-composer-aligned.svg) · [120 列](product-verification/12-actual-aligned-120.svg) · [40 列稳定帧](product-verification/14-actual-aligned-40-settled.svg) · [原始 ANSI](product-verification/actual-session.ansi)

最新交互补验：实际命令菜单完成重命名且保留中文普通草稿；鼠标打开模型/命令入口；无效 profile 显示错误且保留输入；40 列模型长文本光标可见，200 列内容随中栏拉伸。

后续 release 性能检查通过：120×40 render p95 0.43 ms，160×50 render p95 0.54 ms，input-to-render p95 0.56 ms，缓存约 3.84 MB；RSS 仍不可用。这是本机 harness 测量，不代表真实服务端延迟。

最终退出补验：32–35 记录全新工作区中未提交的两行中文草稿，Ctrl+Q 正常退出，重启完整恢复且只创建一个会话；36 确认首次提交自动创建成功后清除创建中提示，并显示实际配置状态与 /model 入口。所有手动 PTY 已正常退出。

## 最终自动化检查（后续功能集成）

- `cargo test -p bone-tui --all-targets --all-features --locked`：96 项通过；1 项常规忽略的 release 性能检查已另跑通过。
- `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`：通过。
- 最终源码包含登录请求身份、会话模型标签隔离、模型/重命名输入隔离、详情入口、回答保存、退出幂等创建和晚到创建回执保护。
- 整体产品交付仍未标记完成：等待 BONE 独立登录后，继续人工验收真实模型回复、停止、重试与问题回答。

## 完整方案补齐续审

交付审计见 [DELIVERY-AUDIT.md](DELIVERY-AUDIT.md)。不能再将余项概括为只差登录：根据原 INTERACTION 逐项补齐文本选择/撤销、会话候选浏览、详情选择与阅读锚点。

44–60已通过实际PTY验证选择替换、撤销重做、拖选、已滚动输入定位、按词移动，以及候选浏览不切会话、Enter打开、滚轮不切会话。模型配置已用App typed facts区分running/saved，避免应用失败时误示新模型正在运行。

详情冷排版与热滚动单独测量：`cargo test -p bone-tui --lib --release --locked reader -- --ignored --nocapture`。正常1MiB与纯换行1MiB均保留完整数据；不能用热滚动数字替代首次排版/极端超预算对象的性能。

## 2026-09-11：统一 /model 接入入口

按用户要求，模型相关能力只保留一个 slash command：/model。模型选择器直接展示模型、编辑接入、添加接入，不要求填写内部 profile ID。账号授权为其中一个子步骤；API 接入可配置名称、协议类型对应地址、Key、模型 ID。移除/login，所有修复提示回/model。

密钥使用遮罩字段和redacted事件载荷，派发后从表单清空；保存失败明确重填，不误导留空保留。保存通过App公开接口分阶段进行，并重新核对持久事实；失败不假装回滚。正在运行的旧API客户端可能仍使用旧Key，保存后按公开running事实提示重启App重载。保存表单的迟到结果只刷新当前scope，不覆盖新表单或启动授权。编辑同一模型的接入保留原reasoning options。

此前Needs login仅代表当时选择的ChatGPT验证接入，不能作为BONE唯一接入方式或TUI整体前置条件。实际远端验证可用任一受支持的有效接入。
