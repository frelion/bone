# 0.7.0 输入与运行反馈复现

实测安装版 `bone 0.7.0`，启动 SHA256 `e2fc28e5688a97e9cf28c6a27c8deeb838c06d25b03cb4e0d7722b6a4fdcc608`。同一真实 PTY 工作流在 80×24、120×40 各完成一次，结果记为 **OBSERVED**；它们不是通过验收。

证据：[80×24](pty/before/80x24/feedback-flow.html)、[120×40](pty/before/120x40/feedback-flow.html)。JSON 保留终端 cell、SGR、硬件光标显示状态、SQLite 暂停/任务状态和请求/事件数量。所有布局帧采于退出前；模型由 loopback SSE fixture 驱动，shell 在隔离工作区实际运行。

## 具体缺陷

| 优先级 | 可观察结果 | 实际状态与影响 |
|---|---|---|
| P1 | `Root has delivered, background shell is still silent and active A` 主状态显示“输入 … 已交付 · /delivery 查看 · 0s” | SQLite 仍有 1 个 Running 子任务，真实 silent shell 尚未产生结果或完成文件。末条前台交付覆盖后台活动。 |
| P1 | `Silent model A/B`、`Background silent shell A/B` 均没有活动动画 | 静默请求和静默 shell 正在执行，两帧无 spinner；首个模型请求阶段主行仍写“输入已接收 · 等待纳入执行”，不能看到当前动作。 |
| P1 | 长中文草稿中间及末尾 `cursor_visible=false`；输入没有上/下边框；F6 后没有“草稿只读”标题 | 140 个中文字符自动换行，实际编辑仅用反色格；阅读和编辑焦点缺少显式区分。发送后的内容并未丢失，此处是焦点和光标可见性缺陷。 |
| 用户要求违背 | 正文出现“你 · 已接收/已纳入”和“Agent · 输出中（未交付）/Agent”标题 | 角色标签占正文行；两种尺寸均复现。 |

旧版已做到的部分：原始发送文字和接收回执可见；流式预览明确标注未交付；Ctrl+C 后 SQLite paused=true、unknown_writes=1，画面提示核查且没有宣称所有物理进程终止。

## 构造修正

初版 fixture 试图让父输入在自己的未完成委托仍运行时交付；Engine 正确将父任务转 Waiting。该 timeout 是验收构造失配，不能当作生产缺陷。最终证据先让原父输入等待子任务，再发送独立 `FRONT_QUICK_REPLY`；该新输入的真实 delivery 与旧子任务 Running 同时存在，从而真实复现状态覆盖。证据目录仅保留这条修正后的完整流程。
