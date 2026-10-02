# BONE 0.6：终端交互交付

本轮把输入、阅读、运行反馈和恢复连成可操作流程。已安装 `bone 0.6.0`；日常入口 `/opt/homebrew/bin/bone` 指向 `~/.cargo/bin/bone`。

- [逐步浏览终端过程](index.html)：11 个精选场景、75 帧，可切换场景和步骤。
- [真实订阅两轮软件修复](real-software/report.html)：`gpt-6-luna`、原有订阅、同一会话。
- [使用指南](../../tui.md)与[原始需求](../../tui-requirements.md)。

## 交付范围

| 需求 | 本轮结果 | 证据 |
| --- | --- | --- |
| 输入与命令 | textarea 单一编辑状态；视觉行导航、Unicode 选区、粘贴、撤销；`/` 与 `@` 即时候选，插入与执行分开 | slash-inline、multiline-undo、completion、Unicode/窗口测试 |
| 消息与工具阅读 | 消息选择、展开折叠、原文详情及完整复制；工具按原始文本阅读，对话使用 Markdown | reading-detail、80×24/120×40 TestBackend |
| 长操作反馈 | Shell 未退出时 stdout/stderr 可见；模型预览区分未交付；输入/停止可操作 | shell-live、真实订阅两轮实时输出 |
| 提问与新要求 | 明确问题 ID；取消后发独立指令；失效目标不改投其他问题，新问题不接管已有草稿，重开保留草稿的明确回复目标 | question-target、runtime 路由、obsolete-question 回归 |
| 历史与恢复 | SQLite 原文全文搜索；正常对话分页、返回最近窗口；详情/复制从持久记录读取；退出打印完整恢复指令 | persistent-search、exit-resume、跨纯内部记录页回归 |
| 修改与未知写 | Git 读取 staged/unstaged 和未跟踪文件实际内容；未知写展示原始工具参数，记录核查结果后仍保持暂停 | shell-live 核查门禁、Git 内容回归 |

对话窗口限 256 条/8 MiB，单条预览限 128 KiB；原文继续存于 SQLite，超限有明确入口。实时工具观察按调用仅保留 stdout/stderr 各 16 KiB 尾部，过滤旧版本、非当前 call、停止与完成后的帧。

## 技术选择与删减

沿用一个 Cargo package、一个 Session Agent 和内部 Job。新增直接依赖 `ratatui-textarea 0.9.2`、`tui-markdown 0.3.10`；删除手写编辑状态和按屏幕行搜索的第二套机制。

- **View** 持有唯一 textarea 状态及有界阅读窗口。
- **App** 处理用户交互和已有 Engine API；没有新的执行调度器。
- **ToolProgress** 是可选的本地观察快照，最终结果仍由原有工具 future 与 SQLite 提交；没有自定义模型流协议。
- **HistoryMatch** 只承载原文命中的 ID/kind/snippet，不另存历史正文。
- **read_call_event** 从会话现有元数据定位单条持久原文；删除 TUI 中重复的工具事件索引。

Rig 模型连接及其原生消息/请求/工具类型未改。物理写 lease、输入版本检查、未知写保护和最终原文保存规则保留。

## 实际验证

| 验证 | 结果 | 范围 |
| --- | --- | --- |
| Rust | 182 passed、1 ignored | 单元、集成及 3 条 API 边界 compile-fail；ignored 为既有子进程 helper |
| Python | 8 passed | 测试脚本与屏幕重放 |
| Clippy / 格式 / diff | 全通过 | all-targets + all-features |
| 独立 PTY | 22/22 passed | 实际终端、本地工具、SQLite 与恢复；模型协议为可控 fixture |
| 安装后的 release | 3/3 passed | 即时 Slash、Shell 实时双流/暂停/核查闭环、提问目标/历史刷新 |
| 真实订阅 | 2/2 轮交付 | 第一轮 7、第二轮 9 次模型调用，均在各 16 次额度内 |
| 软件功能 | 模型测试 5/5；独立测试 3/3 | HTTP-date、注入时钟、过期归零、整数/空白/非法值；独立测试包含 9 个值/边界子场景 |

完整 22 场景的证据来自主要交互完成后的 debug binary；其后新增的输入采纳身份、额度归属与草稿回复目标保存经源码门禁及最终安装版本的提问场景复验。

源代码门禁及逐文件 hash 在 [gates.json](gates.json)。终端二进制 hash、22 个场景与验证前源码状态在 [PTY 结果](pty/summary.json)。安装位置/hash 与 release 冒烟在 [安装验收](installed/summary.json)。

真实软件场景使用独立临时 Git 项目。Agent 修改 Python Retry-After 解析器，运行 unittest，第二轮处理新增约束；保留[实际 diff](real-software/final.diff)、源文件、模型测试和外部测试。报告有 35 个终端帧及事件归属信息。

首轮观察脚本曾使用 macOS 临时目录的非规范路径，漏掉部分事件驱动的工具启动帧；实际实时 stdout 和最终交付仍捕获。SQLite 后验确认已交付，没有补跑首轮请求。屏幕是 VT 文字重放，网页不还原 ANSI 颜色。

## 尚未交付

这些验证不构成 Pi/OpenCode 的全面产品能力对等，也不证明长期复杂工程的普遍成功率。

- 按文件独立审查面板、搜索原文自动定位到匹配行。
- 会话改名/归档、跨会话搜索、完整首次配置引导。
- 持久主题/键位/鼠标偏好、延后输入队列、可选动作确认策略。
- 历史分支、文件撤销、多模态附件、插件和跨设备能力。

后续以真实使用中的失败和操作阻力选择下一批需求，不再把短协议测试通过当作完整产品体验验收。
