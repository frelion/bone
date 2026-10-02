# BONE 开发终端

## 使用

```sh
bone
bone tui
./target/release/bone tui
cargo run --locked -- tui
# 现有配置默认使用 ChatGPT 订阅 gpt-6-luna
bone --profile subscription tui --workspace /path/to/project
bone --profile subscription tui --session SESSION_ID
```

安装入口用 `cargo install --path . --locked --force` 更新，`bone --version` 应为 `bone 0.5.0`。`./target/release/bone` 只在构建它的目录中存在；在另一个 checkout 或 worktree 中启动时，使用安装后的 `bone`，或完整的二进制路径。无子命令时默认打开 TUI。

正常输入任务即可。用户控制会话，不选择内部 Job。界面默认将对话和工程工具行动放在一起，底部是可恢复的多行编辑器；内部事件归属信息通过 F2 和详情浏览。

## 对照依据

本次对照的是终端开发的交互能力：Codex 的持续编码循环与会话恢复、Claude Code 的多行编辑与历史搜索、OpenCode 的文件引用/命令/外部编辑器、Pi 的终端交互与持久会话。依据各自的官方资料：

- [Codex CLI](https://learn.chatgpt.com/docs/codex/cli)
- [Claude Code interactive mode](https://code.claude.com/docs/en/interactive-mode)
- [OpenCode TUI](https://opencode.ai/docs/tui/)
- [Pi 终端使用指南](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/usage.md)

这份交付覆盖 BONE 的本地终端工作流。表中的实现状态仅描述本项目，不代表与这些产品所有扩展、权限系统、跨设备功能等逐项相同。

| 能力 | BONE 实现 | 验收方式 |
| --- | --- | --- |
| 实时模型输出 | 观察原生 Rig 事件；预览与正式交付区分 | 延迟 SSE 的输出先于持久化完成事件 |
| 打断与补充要求 | 输入优先；旧版本预览清除；Engine 控制取消 | PTY + SQLite 版本/归属校验 |
| 工程过程可见 | 工具启动/结果、Markdown、代码块、diff；F2 原文详情 | 真实内核工具事件、渲染测试 |
| 编辑器 | Unicode grapheme、软换行、多行光标、单词/行编辑、undo/redo、历史 | emoji/组合字符精确编辑与草稿核对 |
| 外部编辑器 | VISUAL/EDITOR；切换终端模式；返回草稿 | 真实 PTY stdin 与 SIGTERM 故障检查 |
| 命令与补全 | Ctrl+P 命令、@文件 Tab/Ctrl+O、模糊选择器 | 不触发模型，Enter 仅选路径 |
| 会话管理 | 标题/ID 搜索、/new、/sessions、独立草稿恢复 | 重启后草稿不自动发送、会话实际切换 |
| 模型选择 | 已配置 profile + 原生 provider:model 引用 | 模型切换保留工作与额度；endpoint/认证来源保持 |
| 用量与限制 | 本次请求 token/call、模式、耗时；/status | 原生 usage，缺失显示未知，费用不猜测 |
| 历史浏览 | 最近记录有界加载；更早记录向前分页；搜索预览 | 向前连续分页、无重复/缺口、跨 session cursor 拒绝 |
| 修改检查与报告 | /diff、/copy、/export 本地 HTML | Git 只读检查、安全 HTML、完整正式答复读取 |
| 终端兼容与恢复 | 宽/窄/微小窗口、NO_COLOR、粘贴保护、退出恢复 | PTY resize/termios/alternate screen、渲染测试 |

## 命令

- `/new` 创建新会话，旧工作保存并暂停。
- `/sessions` 搜索当前项目会话；`/sessions SESSION_ID` 直接打开。
- `/model` 选择已配置 profile；`/model PROFILE` 或 `/model provider:model` 切换。
- `/status` 显示真实执行配置、用量、会话 ID 与未知写。
- `/diff` 异步检查 staged、unstaged 和文件状态，不修改仓库。
- `/files` 打开文件选择器；文件引用发送后，由 Agent 在 Job 中决定读取。
- `/search [文本]` 或 Ctrl+F 搜索当前加载的对话预览。
- `/older` 分页浏览更早的原文。
- `/export` 生成数据目录 `tui/exports/` 下的本地 HTML，界面显示完整路径。
- `/copy` 复制最后一条正式答复；macOS 使用 pbcopy，Linux 使用 wl-copy/xclip/xsel。
- `/editor` 或 Ctrl+G 打开外部编辑器；`EDITOR="code --wait"`、`EDITOR=vim` 等。
- `/details` 或 F2 展开内部活动。`/stop`、`/resume`、`/quit` 控制会话。
- `/help` 或 F1 查看键盘操作。未知命令保留草稿，不发送给模型。

## 内核边界

TUI 只通过 Session 接口发送输入、推进执行、停止、恢复与读取记录。所有 Agent 的理解、规划、摘要、模型调用和工作区工具操作仍归属 Job。界面命令由用户主动操作；查看 Git 或导出报告不消耗 Agent 请求额度。

运行中的模型事件由可选、有界、允许丢帧的观察队列传递；保留原生 Rig JSON 和 job/call/revision 归属。无界缓存、每个 token 写入数据库、观察者阻塞模型调用均被避免。持久化完成结果是最终依据，隐藏推理/加密块不作为正文显示。工具 stdout/stderr 目前在工具完成后呈现，未增加另一套工具流协议。

切换模型先暂停工作，验证新配置后仅改变接入 recipe，不重置历史、输入额度或未知写。跨 provider 的临时切换不沿用旧 provider 的凭据变量。模型切换暂不写回 config.toml；重新启动按正常 profile 参数选择。

草稿文件和导出使用 Unix 0600、原子安装与 fsync；不保存认证信息。目录索引有界且不跟随 symlink。Git 检查禁用外部 diff/textconv/fsmonitor，限制时长和输出；HTML 内容转义并禁用脚本。错误保持可见，恢复历史和未知写均沿用内核规则。

## 验证

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo check --locked --all-features
cargo build --locked
python3 -u -B tests/tui_pty.py --binary target/debug/bone
```

终端验收使用本地 Responses fixture 与合成凭据。覆盖原生流提前可见、旧预览清理、停止、问题路由、Unicode 编辑/历史/搜索、粘贴/补全不发送、命令不绕过内核、会话/模型切换、草稿重启、真实编辑器输入、退出与 SIGTERM 恢复；编辑器挂起时也检查子进程被清理。

[测试渲染快照](results/2026-10-01-tui/preview-120.svg) 来自实际 Ratatui TestBackend，明确标注为测试数据。模型实测与 PTY 仿真分开记录；验收结果见 [运行记录](results/2026-10-01-tui/README.md)。
