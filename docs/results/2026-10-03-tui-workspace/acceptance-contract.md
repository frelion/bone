# 工作区交互：独立 PTY 可观察合同

状态：最终安装版三组综合场景已实测通过，旧安装版锁竞争失败也保留。实际版本、证据和未覆盖边界见 acceptance-results.md；合同中的远端认证/login/CAS 故障不因此自动标记为 PTY 已验。

## 最小常用入口

公开 slash 仅 `/new`、`/model`、`/connect`、`/help`、`/quit`。旧命令可兼容解析，但不出现在 slash 候选；Ctrl+P 只显示常用项及当前实际需要的待答问题/未知写核查。会话为左栏 SQLite 会话列表，不与后台 Job 混为一谈。

Ctrl+方向按空间切换焦点；Tab 在编辑/补全或表单字段内工作。左栏异步返回仅更新数据，不抢当前焦点、草稿或选区。圆角和输入框的实际位置按 VT cell 验证，不能用旧 `┌` 字面或 Unicode 字符数当列位置。

## 关键真实动作

- 打开左栏、在加载期间编辑、取消/返回：精确草稿和光标不被异步结果覆盖；极窄 popover 也能返回原 editor。
- 从有运行/回复目标的会话切到另一会话：源会话先保存并暂停，回去仍保存同一个目标和稿；新会话不能变成后台 job 的别名。
- API A、API B 采用不同 loopback endpoint 和不同模型字符串；真正 Enter 发送后的原生 Rig 请求须实际到各自端点，包含选中的原生模型值。
- 改模型/连接的 UI 操作本身不发模型请求；自定义 endpoint 和默认连接保存后跨 restart 保持，不被 harness 强制 `--profile fixture` 覆盖。
- API 表单提交仅校验本地格式、endpoint 和配置 CAS；界面明确“未实测”。不在 Job 外偷偷 completion 探活。远端 401 在真实 Job 中失败，不自动重试，也不把失败说成可用。
- 错格式/无效 endpoint、登录失败、CAS 失败及 Esc cancel 均保原配置；密钥不出 screen、主 draft/history 或事件原文。secret 表单与主 editor 分开且遮罩。
- API A→B→订阅可在本地使用完全合成 BONE OAuth cache 和 loopback endpoint 时验证协议；如果订阅 UI 只能用真实 endpoint，成功订阅由 root 真实链验证，本地只验证坏/缺 cache 与取消，绝不读个人登录缓存。

## 证据与范围

沿用 `tests/tui_pty.py`，最多增加一个综合场景；复用 compact VT JSON 与同一个 `styled_frame`。正式证据来自实际 80×24、120×40，捕获退出前画面、硬件光标、有效 SGR、SQLite 状态、实际端点请求和默认配置；记录启动 binary hash/源码指纹。

本地范围为合成凭据、脚本 loopback 模型和隔离工作区；真实订阅、个人登录及真实模型由 root 单独负责。报告不得将本地 fixture 结果称作真实模型认证成功。
