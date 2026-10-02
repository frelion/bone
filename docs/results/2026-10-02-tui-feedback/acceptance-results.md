# 独立 PTY 验收：输入与运行反馈

**正式关键验收 10/10 PASS**：`feedback-flow`、`stream`、`concurrent`、`shell-live`、`reader-delivery`，各在真实 80×24 和 120×40 运行。综合场景每个尺寸有 24 项明确断言。全部画面来自退出前 alternate screen；没有用退出后 stderr 拼成布局。

- [证据索引](pty/index.html)、[冻结指纹](pty/final-build.json)
- [旧版具体复现](acceptance-baseline.md)、[可观察合同](acceptance-contract.md)
- [80×24 综合全过程](pty/80x24/feedback-flow.html)、[120×40 综合全过程](pty/120x40/feedback-flow.html)

## 已实测的变化

| 合同 | 实测证据 |
|---|---|
| 发送前草稿、发送后精确原话与回执 | 140 个中文字符和 ASCII 尾标自动换行，中间插入后的完整字符串与 SQLite input 原文逐字相等；未发送时没有请求或 input 事件。发送后原话和接收回执可见。 |
| 编辑与阅读焦点 | 输入框有边界和“输入中”标题；F6 后显示“草稿只读”、隐藏硬件光标；返回恢复同一末尾位置。插入点对应真实下一字符“文”，末尾 caret 在实际尾字符 L 之后，位于输入框内。 |
| 默认正文无角色标题 | 静默模型、流式预览、前台交付与后台活动帧均无“你/Agent”正文标题。 |
| 静默与流式模型反馈 | 两尺寸静默模型 spinner 均为 `⠋ → ⠸`；真实 SSE 预览出现在 durable model_message 前，并标明未交付。 |
| 前台交付与后台活动并存 | 新前台 input 的 delivery.reply_to 确实匹配该 input；旧子任务仍 Running、实际 silent shell 未完成。主状态显示“执行命令”，副行“命令尚未产生输出；执行仍在继续”；spinner `⠙ → ⠸`。 |
| 暂停与未知写 | Ctrl+C 后 SQLite paused=true、unknown_writes=1；两帧没有 spinner，画面提示核查，没有声称所有物理进程已终止，未出现未来 shell 文件或子任务最终输出。 |
| 原功能继续可用 | 并发新要求入后续模型上下文；真实 shell 选区复制后 Ctrl+C 确实暂停；核查取消恢复多行草稿/光标与目标，记录结论后仍暂停；旧读点在新事件和 SIGWINCH resize 后保持且有未读入口；已有长交付可直接读取和完整复制。 |

`reader-delivery` 的完整结果在 resize 后分别为 120×40、80×24；记录的左右边框和四角均完整。

## 版本与复现

正式二进制为 `bone 0.7.1`，SHA256：

`06b369416b51bffa2944ce21fc080d39b68a3402dcc870f63d50ae752e6f18fb`

生产源码指纹：

`428da94e974b2b7d6da9b377c597401ccae6847f87bbc05471cbc6e6c78c24f7`

10 个场景启动时二进制 hash 均相同；整个正式复验期间 binary、生产源码和 harness 均未变动。源码范围及算法见 `final-build.json`；Git HEAD 仅作背景，hash 覆盖未提交源码。

```sh
python3 tests/tui_pty.py --binary target/debug/bone --case feedback-flow --size 80x24
python3 tests/tui_pty.py --binary target/debug/bone --case feedback-flow --size 120x40
```

模型只由合成凭据的 loopback SSE fixture 驱动；shell 在临时隔离工作区实际执行。没有读取个人凭据或向真实模型发请求。本报告不包含 root 负责的真实模型/安装版验收。

## 仍有的具体限制

- 80×24 的四行中文草稿会压缩正文阅读空间；主状态、回执、输入边框和两行操作提示仍占固定行数。
- 静默命令主行会缩短命令摘要；完整命令仍需打开原文/审计。窄屏核查证据中的原生参数需 PageDown 阅读，测试确实滚动后验证原调用。
- 这里验证硬件光标的 VT 显示命令与 cell 位置，没有实机验证不同终端的光标样式、Linux 原生剪贴板或所有物理进程树的终止。
- 流式预览转 durable 正文的精确旧阅读锚点、复杂 Markdown 近似渲染与进程重启后的 inactive 草稿仍不在本轮合同中。

JSON 为真实 VT cell/SGR 观察；HTML 使用记录格宽合并同样式 ASCII，宽字/emoji 独立固定格宽、cursor 独立描边。ANSI 色值是浏览器近似。
