# BONE 开发终端

BONE 0.6 的 TUI 使用 Ratatui + Crossterm，输入编辑采用 ratatui-textarea，Markdown 采用 tui-markdown。对话、文件工具和运行日志在同一阅读区；Job 是 Agent 内部单元。

本轮交付、逐屏操作和验证边界见 [交付记录](results/2026-10-02-tui-product/README.md)及[可浏览过程](results/2026-10-02-tui-product/index.html)。后续需求见 [产品需求](tui-requirements.md)。

## 启动

```sh
bone
bone tui --workspace /path/to/project
bone --profile subscription --model chatgpt:gpt-6-luna tui --workspace /path/to/project
bone --profile subscription tui --session SESSION_ID --workspace /path/to/project
```

安装用 `cargo install --path . --locked --force`，`bone --version` 应为 `bone 0.6.0`。安装后的 `bone` 可以在任意项目目录使用。终端必须为交互式 TTY，`TERM=dumb` 会给出明确错误。

现有 subscription 配置继续使用原有登录，不需要重新登录。切换模型保留会话工作和额度，先暂停，Ctrl+R 显式继续；`/model` 仅修改当前运行，不写回 config.toml。

## 输入与命令

输入 `/` 即显示命令，继续输入筛选；输入 `@` 自动索引项目文件。↑↓选候选，Tab 插入；Enter 选择不完整候选也只插入。命令完整键入或补全后，再 Enter 才执行。例如 `/st` → Tab → Enter；完整键入 `/status` 可以直接 Enter。Ctrl+P 命令菜单中的 Enter 执行选项。

文件补全只插入引用。发送后由 Agent 在 Job 中决定读取。粘贴多行、选候选、草稿恢复均不会自动发送。

| 操作 | 键位 |
| --- | --- |
| 发送 / 换行 | Enter / Shift+Enter、Alt+Enter、Ctrl+J |
| 视觉行导航 / 历史草稿 | ↑↓ / Alt+↑↓ |
| 单词 / 行首尾 | Ctrl+←→ / Ctrl+A、Ctrl+E |
| 选区 / 删除单词、行尾 | Shift+方向键 / Ctrl+W、Ctrl+K |
| 撤销 / 重做 | Ctrl+Z / Alt+Z |
| 外部编辑器 | Ctrl+G；使用 VISUAL 或 EDITOR |
| 命令 / 文件 / 全文搜索 | Ctrl+P / Ctrl+O / Ctrl+F |
| 切换输入、对话、活动焦点 | Tab；已有候选时先确认候选 |
| 选择消息 / 展开折叠 | 对话中的 ↑↓、j/k / Enter |
| 原文 / 完整复制 | 对话中的 d / y；详情中 Ctrl+Y |
| 滚动 / 首尾 | PageUp、PageDown / Home、End |
| 暂停 / 恢复 / 退出 | Ctrl+C / Ctrl+R / Ctrl+Q |
| 帮助 / 活动记录 | F1 / F2 |

Ctrl+C 在输入框有选区时复制，否则暂停。Ctrl+Y 按当前可见界面复制详情、输入选区或选中消息。系统剪贴板使用 macOS pbcopy 或 Linux wl-copy/xclip/xsel。

## 运行、提问与阅读

模型预览明确标记为“输出中（未交付）”。Shell 未退出时显示 stdout/stderr 尾部，最终工具记录替换预览。执行失败、结果未知和成功完成分别显示；输入与停止仍可操作。

输入栏显示回复的具体问题。`/questions` 选择待回答的问题；Esc 取消回复，随后作为新指令发送。新问题不会接管正在编辑的独立草稿，失效目标不会改投另一问题。运行中新要求持久化后，旧结果不能启动过期外部动作。

最近会话按页加载。`/older` 在正常对话区往前翻页，`/latest` 返回最近窗口；草稿保留。Ctrl+F 或 `/search 文本` 搜索完整 SQLite 原文，包含工具命令、路径、结果，独立于当前窗口和换行宽度；选择命中后读取完整原文。

对话窗口最多 256 条、8 MiB，单条预览最多 128 KiB。超限会说明原文入口；d/y 和搜索从 SQLite 读取完整记录。工具日志按原样阅读，Markdown 用于对话内容。

## 命令

| 命令 | 用途 |
| --- | --- |
| `/new`、`/sessions [ID]` | 创建、搜索或打开当前项目会话；旧工作先保存并暂停 |
| `/model [PROFILE 或 provider:model]` | 选择配置或原生模型引用 |
| `/status` | 模型、执行额度、会话信息和未知写入 |
| `/questions`、`/reply ID` | 选择明确的回复目标 |
| `/stop`、`/resume` | 暂停、显式恢复 |
| `/files` | 文件引用选择 |
| `/search [文本]`、`/older`、`/latest` | 全记录搜索、历史翻页、返回最近窗口 |
| `/diff` | 异步读取 staged/unstaged 修改及未跟踪文件实际内容 |
| `/reconcile [CALL_ID 核查结论]` | 查看未知写原始参数；记录实际核查结果 |
| `/copy`、`/export` | 复制原文、导出本地 HTML 对话与行动报告 |
| `/mouse` | 切换鼠标滚动与终端原生文本选择 |
| `/editor`、`/details`、`/help` | 外部编辑、活动记录、帮助 |
| `/quit` | 保存草稿、暂停工作、恢复终端；打印完整恢复命令 |

未知写入先查看实际文件、Git diff、测试或进程结果。记录核查结论后仍保持暂停，Ctrl+R 才继续。记录结论不重放未知操作，也不能绕过仍活跃的物理写 lease。

## 技术边界

TUI 通过 Session 级 Engine 发送输入、执行、停止、恢复和读取记录。所有 Agent 模型与工作区工具仍归属 Job。用户的 Git 检查、复制、报告导出是显式界面操作。

模型观察保留 Rig 原生事件。工具观察仅提供每调用最新有界尾部快照，不替代最终持久结果。观察可丢帧且不阻塞调用；旧 revision、已结束 call 和停止后的预览会拒绝。每调用仅有一份持久原文，界面不写入每个 token。

草稿与报告使用原子写入及 Unix 0600；不保存认证信息。文件索引有界且不跟随 symlink。Git 检查禁用外部 diff/textconv/fsmonitor，并限制时间与输出；未跟踪文件超出预览限制会标注。HTML 内容转义且不执行原文脚本。

## 验证

```sh
cargo test --locked
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo build --locked
python3 -u -B tests/tui_pty.py --binary target/debug/bone
python3 -B -m unittest discover -s tests -p 'test_*.py'
```

真实 PTY 使用本地协议 fixture 验证输入、屏幕、实际文件工具、SQLite 归属和终端恢复；真实订阅的软件修复另行记录。测试范围与尚未交付的需求分开列在 [交付记录](results/2026-10-02-tui-product/README.md)。
