# TUI 交付与验收 · 2026-10-01

本目录将渲染样例、离线终端验收与真实订阅调用分开记录。

## 结果

- Rust 全部回归：163 项通过，1 项忽略（既有 subprocess helper）。
- Clippy `--all-targets -D warnings`：通过。
- 全部可选 feature 编译：通过。
- Debug 与 Release 的真实 PTY 验收：各 15 / 15 通过。使用合成凭据和本地 HTTP/SSE fixture。
- 优化后的 Release 二进制已构建，启动命令：`./target/release/bone tui`。
- 现有 Codex 订阅 + gpt-6-luna：两轮只读 TUI 实测通过，3 次模型调用、1 次 read_file，全部归属同一 Job；无未知写，退出恢复终端。

## 合并前复验

2026-10-02 合并前复验：清理后的 Rust 回归 167 项通过、1 项忽略，Clippy 通过；命令、会话切换和旧输出清理三个 PTY 场景通过。公开行动报告的脱敏导出回归通过。移除无用渲染分配、统一 Markdown 滚动计算，并将工具提议修改与实际执行结果明确区分；Release 安装入口已更新。10 月 1 日的完整终端及订阅实测结果仍按原日期保留。

## 真实模型记录

[会话 HTML](live-session.html) 是 TUI 的 `/export` 从 SQLite 原文生成的报告。

[独立核对结果](live-verdict.json) 包含会话 ID、两轮答复、模型/工具调用数和终端恢复结果。任务只读 Cargo.toml：第一轮确认 package 名称和版本；第二轮沿用上下文回复 `bone@0.5.0`。这证明 TUI → Engine → Job → Rig/工具的两轮实际接入，不用于证明复杂软件工程模型能力。

验证使用隔离 BONE 数据目录，显式复用现有 Codex 登录，未复制任何凭据内容。原始 SQLite 位置见 verdict。现有项目代码没有被模型改写。

## 终端验收场景

`python3 -u -B tests/tui_pty.py --binary target/debug/bone` 和 `--binary target/release/bone`：

1. paste：多行粘贴只进入草稿，Enter 才提交；大小窗口切换。
2. concurrent：模型运行期间接收新输入并出现在下一次原生请求。
3. pause：Ctrl+C、Ctrl+R 与 `/stop`、`/resume`，暂停不继续调用。
4. question：内部 Job 的提问进入普通对话，普通回答续做，归属正确。
5. stream：真正延迟 SSE 在持久完成记录之前显示原生输出。
6. stale：新输入到达后旧预览消失，新版本交付。
7. stream-stop：暂停清除流预览，取消结果不成为正式完成。
8. commands：未知命令保留；UI 命令不产生 Agent 输入；编辑器草稿重启可恢复。
9. completion：Tab/Enter 选中文件只插入引用，第二次 Enter 提交。
10. unicode：中文/emoji/组合字符编辑、历史召回、搜索的内容核对。
11. sessions：模型切换保留暂停、HTML 导出、/new 和实际打开原会话。
12. interactive-editor：编辑器真实读取终端 stdin，返回后草稿不自动发送。
13. editor-signal：编辑器挂起时 SIGTERM 清理子进程并恢复 termios。
14. failure：provider 错误不冒充成功，终端可退出恢复。
15. signal：SIGTERM 退出恢复 raw mode 和 alternate screen。

SQLite 中的版本、原始输入、模型/工具归属与终端当前屏幕联合检查。测试保留 ANSI/Unicode 的屏幕重放校验，避免将早已消失的文本误判为仍显示。

## 可见快照

[120 列 SVG](preview-120.svg)、[80 列 SVG](preview-80.svg) 以及同名 TXT 来自实际 Ratatui TestBackend 的单元格缓冲区，包含工具记录、diff、模型预览与多行输入。它们使用明确标注的合成场景，不是截图形式的真实模型实测。

## 已修复的验收失败

- 用户终端在另一个 worktree，相对构建路径不存在；PATH 入口仍是 0.2.1：安装 0.5.0 到 `~/.cargo/bin/bone`，将实际 launcher 指向新版并保存旧入口备份。裸 `bone` 默认打开 TUI，两个针对性 CLI 检查通过；从用户终端所在目录通过实际 PATH 启动并正常退出。详情见 [启动修复记录](launch-fix.json)。
- 初始外部编辑器空草稿没有创建 tui 目录：显式创建目录。
- 编辑器返回时 Terminal::clear 查询光标与事件读取冲突：保留 fullscreen 后端，用 resize 重置渲染缓冲。
- 模态粘贴修改隐藏草稿、窄屏 `/details` 未切换、命令选择失败丢失草稿：修正各自的状态处理。
- 模型临时覆盖可能丢失自定义 endpoint 或沿用其他 provider 的凭据：使用 Rig 原生构造保留同 provider 配置，跨 provider 使用独立 recipe。

源码与操作说明：[终端文档](../../tui.md)。当前平台实测为 macOS，本地 Rust/PTY 测试不代表已经在所有 Linux 终端或全部 provider 上实测。
