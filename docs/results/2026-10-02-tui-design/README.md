# BONE 0.7 TUI 设计交付

本次在 `codex/tui-workflow` 实现设计实验的方向 A：工作段落与按需证据。使用现有 Ratatui、Crossterm、ratatui-textarea、tui-markdown；没有新增依赖、模型包装或 UI 执行内核。

可浏览的实际终端与任务过程见 [overview.html](overview.html)，操作说明见 [docs/tui.md](../../tui.md)，取舍来源见 [设计实验](../../design-lab/2026-10-02/index.html)。

## 已实现

- 单纵列正文，模型与用量移到 `/status`。成功工具实际占一行；运行时 stdout/stderr 短预览，失败退出码与明确错误优先。完整结果先于审计参数。
- F6 输入与阅读；Tab 编辑与候选；Esc 返回父层和原位置。Ctrl+C 在任意焦点暂停，Ctrl+Y 独立复制。详情、帮助和模态列表采用全宽阅读层，避免中文宽字符跨边界吞掉边框。
- 问题到达只提醒。`/questions` 或 `/reply ID` 明确绑定目标，目标栏显示问题短文；`/message` 切回新要求。各目标保存进程内完整编辑状态。
- 核查用独立表单；查证与取消还原原草稿、目标和阅读层。审计中的 Enter 不提交核查结论；记录后仍暂停，Ctrl+R 才继续。
- 新记录不抢旧阅读位置；原文缩放与分层返回保位；`/delivery` 直接查询最近持久交付。`/latest` 刷新保留所有目标草稿。

## 验证与已修缺陷

验证结果及源码、二进制指纹见 [gates.json](gates.json)，独立操作结论见 [independent-acceptance.md](independent-acceptance.md)。

真实 PTY 两种尺寸各 26 个场景；协议来自本地 fixture，终端、Shell、文件、SQLite 归属、物理中断和终端恢复实际执行。测试沿用一个 `tests/tui_pty.py`，新增 4 个场景，更新旧场景的交互合同；没有新增一套 UI 测试框架。

独立审查实际抓到并修复：失败退出码漏显示、把测试 `ok` 误选为失败原因、`/latest` 丢光标、审计父对象漂移、核查审计 Enter 意外提交、审计粘贴改变隐藏焦点，以及 Ratatui 宽字符跨浮层边界吞掉角和竖边。相关语义回归放在既有 `tests/unit/tui*.rs`。

屏幕采集也修复了 VT 宽字符覆盖、擦除和 resize 通知，退出前采集。终端文本重放没有复现 ANSI 配色，不把它称为原生终端截图。

## 安装后二进制的真实模型任务

复用既有 Codex 登录和 `chatgpt:gpt-6-luna`，通过实际安装的 `bone` 在隔离 Git 仓库中修复反向过期判断。检查中插话要求先解释和提问；回答之前源码保持原样，明确回复后只修改 `auth.py`，原有 12 项测试独立复验。所有模型与工具记录均带 Job 归属。

查看 [完整屏幕、对话、Job 与行动日志](live-software/report.html)、[原始事件](live-software/events.json)、[实际 diff](live-software/final.diff) 和 [独立测试](live-software/independent-tests.txt)。重现入口为 [run.py](live-software/run.py)；它会调用真实模型，只用于已有授权和凭据的环境。

这里是一个小型、可复现软件任务，证明的是实际交互链路；没有用它推导大型工程任务成功率。费用不可获得，记录为 `null`。

## 剩余边界

- 模型流预览转换为最终交付时，不保证所选预览的阅读锚点连续；`/delivery` 可直达持久结果。工具观察按调用身份续接。
- 复杂 Markdown 字节锚点仍为近似；原文、普通正文和已复现的表格边框问题有缩放回归。
- 非当前目标草稿仅保存在本次进程内；退出只持久化当前草稿及目标，核查时保存核查前原稿。编辑撤销栈不跨重启持久化。
- Linux 原生剪贴板、不同终端主题和所有终端实现尚未逐项实机测试；本轮为 macOS PTY 和 TestBackend 验证。

旧版失败屏保留在 [before/](before/failure-0.6.html)。完整源码、报告和测试保存在本分支；本机可在项目目录运行 `bone`，或使用：

```sh
bone --profile subscription --model chatgpt:gpt-6-luna tui --workspace /path/to/project
```
