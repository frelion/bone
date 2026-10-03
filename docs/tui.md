# BONE 开发终端

BONE 0.8.0 使用 Ratatui、Crossterm、ratatui-textarea 与 tui-markdown。左侧切换会话，右侧写要求、读结果和查看证据。Job 由 Agent 管理。

## 启动

```sh
bone
bone tui --workspace /path/to/project
bone tui --session SESSION_ID --workspace /path/to/project
```

安装：`cargo install --path . --locked --force`。`bone --version` 应显示 `bone 0.8.0`。必须使用交互式终端；管道输入用 `bone chat`。

## 连接与模型

`/connect` 选择已保存的连接，或添加连接：

- **ChatGPT · 使用现有 Codex 登录**：复用本机登录，凭据不复制。确认连接名与模型即可。
- **ChatGPT · 登录**：显示浏览器地址与设备码，等待登录完成。Esc 取消，当前连接与草稿保留。
- **添加 API 连接**：从 Rig 原生 provider 中选择，依次填写连接名、原生模型名、完整 endpoint、API key。密钥遮罩，留空使用该 provider 的环境变量或原生认证。

`/model` 修改当前连接的模型。名称直接交给 Rig，无模型白名单；例如 Ollama 名称中的冒号仍属于模型名。同一连接保留 endpoint、协议、凭据来源与请求参数。换 provider 使用 `/connect`。

连接和模型保存为默认选择，下次直接 `bone` 即可。显式传入 CLI `--profile` 或 `--model` 仍优先。右上方始终显示当前连接和模型。

设置阶段校验本地格式与配置冲突；认证是否有效由首次实际 Job 请求验证，没有额外模型探活。API key 单独存于 BONE 数据目录的 `profiles/<name>/api-key`，文件权限 0600，绑定 provider 和 endpoint；不进入 Profile、聊天草稿或事件配置。登录失败、取消或配置冲突保留原连接；冲突后重新打开 `/connect`。

切换前暂停旧工作并保留上下文和额度；Ctrl+R 继续，也可直接发送新要求。未知写入仍需先核查，不自动重放。

## 最少入口

公开 slash 只有 5 个：

| 命令 | 用途 |
| --- | --- |
| `/new` | 新会话，保存并暂停旧工作 |
| `/model` | 修改当前模型 |
| `/connect` | 切换连接、登录或添加 API |
| `/help` | 键盘帮助 |
| `/quit` | 保存、暂停并退出 |

输入 `/` 筛选，Enter 或 Tab 只插入候选，再 Enter 执行；完整键入命令可直接 Enter。Ctrl+P 打开常用操作，并在确有待答问题或未知写入时增加对应操作。旧诊断命令兼容手动输入，不进入公开候选。

## 焦点与编辑

| 操作 | 键位 |
| --- | --- |
| 会话栏 / 返回原主焦点 | Ctrl+← / Ctrl+→ |
| 正文阅读 / 输入 | Ctrl+↑ / Ctrl+↓ |
| 会话选择 / 打开 | 左栏 ↑↓ / Enter |
| 发送 / 换行 | Enter / Shift+Enter、Alt+Enter、Ctrl+J |
| 单词 / 行首尾 | Alt+←→ / Ctrl+A、Ctrl+E |
| 选区 / 删除单词、行尾 | Shift+方向键 / Ctrl+W、Ctrl+K |
| 撤销 / 重做 / 历史输入 | Ctrl+Z / Alt+Z / Alt+↑↓ |
| 文件引用 / 搜索 | @ 或 Ctrl+O / Ctrl+F |
| 项目修改 / 外部编辑 | Ctrl+D / Ctrl+G |
| 结果原文 / 审计 / 复制 | 正文 d / 详情 D、F2 / Ctrl+Y |
| 暂停 / 恢复 / 退出 | Ctrl+C / Ctrl+R / Ctrl+Q |

窄于 64 列时会话栏收起，Ctrl+← 临时打开，Ctrl+→ 或 Esc 回到原焦点与阅读位置。异步加载会话只更新列表，不抢焦点。切换会话先保存草稿与回复目标；未完成工作保持暂停。

表单每个字段有独立编辑器：Enter 下一项或保存，Tab / Shift+Tab 切字段，Esc 取消。窗口不足以显示字段时提示扩大窗口，并停止编辑与保存；Esc 仍可返回。表单、帮助和原文层返回时保留父草稿、光标、选区与撤销记录。核查期间先记录或取消，再切换会话。

## 运行与阅读

原话用左侧细竖线区分；普通正文不显示“你 / Agent”标题。编辑时显示真实终端光标；阅读时草稿只读。底部显示实际调用、耗时与回执，等待回复没有执行动画；后台调用不会被较早交付遮住。

问题到达只提醒。选中问题按 Enter，或 Ctrl+P 选择“回复问题”，明确绑定目标；菜单中的“写新要求”返回原稿。Esc 返回层级，不解绑、不恢复执行。不同目标在本进程内保留各自草稿；退出保存当前草稿及目标，核查期间保存原稿。

成功工具压为一行，Enter 展开，d / Ctrl+Y 获取持久原文；失败先显示退出码与错误。运行日志及模型流标记未交付，正式结果以 SQLite 记录为准。新记录不抢原阅读位置。

预览最多 256 条 / 8 MiB，单条 128 KiB；完整记录用 Ctrl+F 搜索。复杂 Markdown 的阅读位置仍是近似，流预览转为持久结果时不保证位置连续。宽度变化按实际终端格处理，支持 NO_COLOR 与中文编辑。

设计约束见 [DESIGN.md](../DESIGN.md)。本轮真实终端、协议连接与订阅任务的可浏览证据见 [工作区交互报告](results/2026-10-03-tui-workspace/overview.html)。
