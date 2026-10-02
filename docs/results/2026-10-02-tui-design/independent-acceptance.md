# 方向 A 独立 PTY 验收

最终构建在 **80×24 与 120×40 各通过 26 个场景，共 52 / 52 PASS**。退出前的逐步终端帧、SQLite 状态计数和调用计数可从 [场景索引](pty/index.html)查看；完整数据为各场景的 JSON。验收实现位于 `tests/tui_pty.py`，没有另建一套测试程序。

这次结果只证明下述脚本化交互与执行合同。它使用真实 PTY、真实文件工具和 shell，但模型为本机 scripted Responses 服务，认证为合成 key，workspace/data 均为临时隔离目录。没有读取个人凭据、调用真实模型、修改 production 工作区或提交 Git。真实模型软件任务由主任务另行记录，不能算作本份独立验收的结果。

## 构建身份

- Binary：`target/debug/bone`，版本 `bone 0.7.0`。
- 启动 SHA-256：`2ecd69d3717e865ba303e30c6834d3f3a99ca6023d3c0ba96ba9cfcb5f7adcac`。
- 源码指纹：`f9a8edb98eef74dbaba33fecbacfc9bddba78cb31f3aa60bd73d975e05f751ae`，覆盖 `Cargo.toml`、`Cargo.lock` 与所有 `src/**/*.rs` 的路径及内容。
- 每个场景在启动进程前记录 binary hash；52 个启动 hash 一致。结束后复核源码、binary 和验收脚本均未变化。具体时间、Git HEAD、脚本指纹和覆盖文件列表见 [构建身份记录](pty/final-build.json)。

## 实测合同

| 合同 | 实际操作与断言 | 证据 |
| --- | --- | --- |
| 失败不称就绪 | HTTP 422 后主状态有失败，进程仍可操作，状态不显示就绪 | [failure 80](pty/80x24/failure.html)、[120](pty/120x40/failure.html) |
| 有选区仍能停止 | Shift+Left 真正选中末尾 `ft`，先用 Ctrl+Y 精确复制证明选区存在；Ctrl+C 后 SQLite paused=true、unknown_writes 非空，真实 shell 未完成其尾部写入，草稿保留 | [shell-live 80](pty/80x24/shell-live.html)、[120](pty/120x40/shell-live.html) |
| 核查保留编辑器 | 独立核查表单 → Ctrl+D 证据 → Esc 表单 → Esc 取消；在原多行草稿中间插入 `!`，文本与原光标目标一致；已有具体问题回复目标也恢复 | [reconcile-reply 80](pty/80x24/reconcile-reply.html)、[120](pty/120x40/reconcile-reply.html) |
| 核查不重放动作 | 表单内 Ctrl+R 不恢复；提交真实观察结论后 unknown_writes 清空，仍 paused，模型调用数不增加 | [shell-live 80](pty/80x24/shell-live.html)、[120](pty/120x40/shell-live.html) |
| 问题不抢输入目标 | 单个新问题到达时草稿保留，普通输入的 reply_to 不指向问题；两个问题通过 /reply 明确选择，/message 明确切回新要求 | [question-independent](pty/80x24/question-independent.html)、[question-target](pty/80x24/question-target.html)；两尺寸均通过 |
| 分层返回保留原读点 | F6 选工具 → d 完整结果 → D 审计 → Esc 结果 → Esc 原对话；随后 y 复制的文字与完整原文件逐字一致，包括全部换行 | [reading-detail 80](pty/80x24/reading-detail.html)、[120](pty/120x40/reading-detail.html) |
| 运行证据没有未来结果 | shell 未退出时可见 stdout/stderr，持久 tool_result 尚不存在；打开并复制实际行动证据，未出现未来 exit 结果或预设交付 | [shell-live 80](pty/80x24/shell-live.html)、[120](pty/120x40/shell-live.html) |
| 新内容与 resize 不夺读点 | 阅读原输入时新交付到达，原阅读锚点仍可见，显示新内容入口；resize 后主分隔线铺满新列数、最后一行在新高度显示阅读状态，同一锚点保留 | [reader-delivery 80→120](pty/80x24/reader-delivery.html)、[120→80](pty/120x40/reader-delivery.html) |
| 工具默认密度与错误 | 成功 read_file 为一行摘要，正文不泄漏；失败 shell 的 stderr 含开始/错误/结束三行，稳定完成摘要直接显示中间关键错误与 exit 17 | [tool-density 80](pty/80x24/tool-density.html)、[120](pty/120x40/tool-density.html) |
| 长交付可直达 | /delivery 打开已有最近交付，Ctrl+Y 可复制 100 行答案尾部；阅读不增加模型调用或事件 | [reader-delivery 80](pty/80x24/reader-delivery.html)、[120](pty/120x40/reader-delivery.html) |
| slash 候选只插入 | `/sta` 的候选 Enter 与 Tab 都只插入 /status 草稿；候选消失后第二个 Enter 才打开本地状态，模型调用与 input 事件均为零 | [slash-inline 80](pty/80x24/slash-inline.html)、[120](pty/120x40/slash-inline.html) |

其余场景复验了 Unicode/组合字符编辑、多行撤销与重做、粘贴、文件引用、并发追加要求、模型流与取消、外部编辑器与信号清理、会话切换、历史检索、退出恢复命令。所有 26 个名称及两尺寸结果见 [场景索引](pty/index.html)。历史检索的长会话由已验证的原生事件形状复制出 70 组后续记录，随后使用真实 PTY 搜索 SQLite 原文；没有宣称跑过 70 次模型会话。

## 布局证据的可信范围

布局只采集进程退出前的 alternate screen。终端属性恢复与退出恢复命令单独断言，退出后的 stderr 不会叠入布局。验收重放修复了实际发现的 CSI E/F/X 和擦除、中文宽字符交叉覆盖问题，并以同一脚本中的针对性自检验证。resize 在 ioctl 后明确发送 SIGWINCH。

首轮发现的失败退出码缺失已修复。后续逐帧检查又发现 Ratatui 差分跳过被背景中文跨越的弹层左边框，最终构建改用全宽阅读层。最终读取工具与长交付的四个关键帧均检查了完整左右边框，两个终端方向的 resize 后也检查了实际重绘。此前的首轮和修复前资料保存在 `pty/initial-failures/`、`pty/pre-overlay-fix/` 等子目录，仅供追溯；本结论使用 `pty/80x24/` 和 `pty/120x40/` 的最终帧。

## 仍不够顺手的细节

80 列时状态与临时提示共用一行，长提示会被截尾。核查取消帧中“执行仍暂停”落在行外，尽管 SQLite 已证明它仍暂停；失败工具正在等待下一轮时，状态行末尾的输入说明也可能被截断。更短的提示和明确的优先级会让这些状态更容易确认。

回复目标现在有问题摘要，但在 80 列仍是短 ID、被省略的问题文本和 `/message` 指令。路由正确，切回原新要求草稿仍需要记住这个命令；这是当前常见操作的额外成本。

当前模型、profile、用量与完整会话身份需要进入 /status 查看，主屏优先保留项目、状态与输入。这是本次设计的取舍，脚本没有将其当作失败；长时间工作时，核对模型仍会增加一次操作。

回复草稿切换与核查取消验证的是进程内保存；非当前草稿仍只在进程内，本份没有验证它们跨重启恢复。也没有覆盖正在阅读模型预览时，预览转正式交付的精确行锚点。

复杂 Markdown 的近似终端呈现、鼠标选择、Linux 原生剪贴板与主题、多终端实现的并排体验，均没有在本份验收中做实机验证。真实模型回答质量由主任务另行记录；完整 P1/P2 功能不在本次宣称范围内。52 个通过结果可以支持已测闭环，不能直接推导为所有日常操作已经成熟。
