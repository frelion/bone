# 输入与运行反馈：PTY 可观察合同

在原 `tests/tui_pty.py` 中只增加一个 `feedback-flow` 综合场景，并扩充已有 `stream` 和 `concurrent` 断言。最终复验范围为这三项与既有 `shell-live`、`reader-delivery`，各运行 80×24、120×40。

- 默认正文不出现“你/Agent”角色标题；发送前为可编辑草稿，发送后保留精确原话和可见回执。
- 静默模型和真实静默 shell 都有当前动作反馈，并捕获至少两个不同 spinner 帧。
- 独立前台输入的真实 delivery 不覆盖尚在运行的旧子任务状态；不绕过 Engine 的父任务等待规则。
- 原生 SSE 文本在 durable model_message 前可见，仍明确是未交付输出。
- Ctrl+C 将 SQLite 置为 paused、记录未知写，spinner 停止；画面不宣称所有物理进程都已终止，不创造未来 shell 或子任务结果。
- 编辑框与正文有可见边界、输入/只读标题；F6 阅读隐藏硬件光标，再返回编辑恢复位置。
- 140 个中文字符自动换行，验证插入点对应真实下一字符、末尾光标位于实际尾字符之后且在输入框内。
- 已有真实 shell 选区复制、暂停、核查取消还原草稿/插入点、核查结论仍 paused 的合同继续通过；窄屏核查参数允许实际 PageDown 阅读。
- 已有阅读锚点、未读入口、SIGWINCH resize 和完整长交付读取合同继续通过。

所有截图来自退出前 alternate screen；JSON 保留 VT cell、宽字续格、有效 SGR、硬件光标与 SQLite 观察。HTML 根据记录格宽绘制，不重新计算 Unicode 宽度。颜色是 ANSI 浏览器近似，终端 cell 位置以 JSON 为准。

范围边界：loopback SSE 与真实本地 shell；不调用真实模型，不读取个人凭据，不证明跨平台终端/原生剪贴板或所有进程树已退出。
