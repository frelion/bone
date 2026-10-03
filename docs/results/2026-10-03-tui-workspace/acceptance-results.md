# 工作区交互：独立验收

结论：**最终安装版三组综合 PTY 全部通过**。仅使用 `/Users/zzhang/.cargo/bin/bone` 0.8.0，SHA256 `1fd4ef9d339e0bfc13e979f245c4567526d26c04fcfd42b76d7eac2eb14842d3`。每组开始/结束的生产源码、验收脚本指纹一致；[完整指纹](acceptance-fingerprint.json)。

## 实测范围

| 二进制 / 场景 | 终端与模式 | 结果 |
| --- | --- | --- |
| 最终 installed release / workspace-flow | 80×24 / NO_COLOR | PASS，24 条语义断言、18 个退出前帧 |
| 同上 | 120×40 / NO_COLOR | PASS，24 条语义断言、18 个退出前帧 |
| 同上 | 80×24 / 彩色 | PASS，24 条语义断言、18 个退出前帧，553 段有效前景/背景 SGR |
| 较早 debug `20e1d336…` / slash-inline、sessions、feedback-flow、shell-live、reader-delivery、reconcile-reply | 各 80×24 / NO_COLOR | 6 / 6 PASS；此项未冒称最终安装版回归 |

唯一新增场景沿用既有 PTY 和 scripted_responses fixture。三组各自使用隔离 data/workspace、两个独立 localhost endpoint、合成密钥及自写 BONE OAuth cache；不读取个人凭据，不向真实模型请求。每组真实模型协议请求为 A 端点 4 次、B 端点 4 次。

[80×24 过程](pty/80x24/workspace-flow.html) · [120×40 过程](pty/120x40/workspace-flow.html) · [彩色过程](pty/color/80x24/workspace-flow.html) · [旧六项范围](acceptance-compatibility.json)

## 通过的实际合同

- API A 的 Job 请求进入 A，模型为 `fixture`；表单创建 B 后 Job 进入 B，模型为 `arbitrary-org/model:v2`。B 内改为完整原生字符串 `native/any:opaque-model-v9`，实际请求保持该值和 endpoint。无 `--profile` restart 后默认仍为 B 与该字符串；切到合成订阅连接后，原生 ChatGPT 协议实际交付。每次交付必须属于当前 SQLite session，不能把另一会话的事件算作成功。
- 会话列表有 1 个 SQLite 会话、2 个 Job；后台 Job 不成为侧栏会话。新建第二会话前，原会话保存并暂停；返回后同一问题目标、两行中文草稿与插入位置保持。Ctrl←/→、Ctrl↑/↓ 有真实硬件 caret 显隐；回到输入后在原插入点添加 `!` 的精确草稿证明位置恢复。
- `/sessions` 取消后输入，异步索引返回期间草稿和编辑焦点保持。未注入 SQL 阻塞，因此不宣称慢存储故障注入覆盖。
- 无效 endpoint 留在表单且配置字节不变；密钥步骤取消恢复主草稿及旧配置。连接/模型保存、创建会话和焦点动作本身不发探活请求。保存提示明确“首次请求验证认证”。API key 原生输入遮罩且与主草稿分离；两个合成 secret 未出现在两段进程完整 VT、持久事件或输入历史中。
- 26×10 侧栏 popover 可返回原编辑稿；模型 modal 明确“扩大窗口编辑 / Esc 返回”。不可见字段按 Enter 不产生事件、请求或配置变更，Esc 返回原稿。此项证明安全返回与不可见提交保护，不称极窄表单可完整编辑。
- provider 候选和 API 表单标题不再重复 `openai/openai`；回复目标保留，重复选择回执及 slash 提示已收敛。旧 slash 补全回归验证可见常用项恰为五个，候选 Enter/Tab 只插入，完整 `/help` 再 Enter 才打开帮助。

正式两尺寸在 NO_COLOR 环境，颜色字段均为空；单独彩色组记录了实际 SGR。所有布局证据采退出前 alternate screen，JSON 保留真实 VT cell、cursor、SGR、SQLite 状态和端点请求计数。HTML 复用同一 styled_frame，宽字按记录格宽布局，连续 ASCII 同样式格合并；浏览器调色板是终端颜色的近似展示。

## 已复现的生产失败与修复后范围

前一 installed release `987052c4…`：80×24 在订阅输入步骤退出，120×40 在第二会话首次 API A 输入退出，均为 `session commit failed: database is locked`；彩色组当时通过。三个工作区没有共享 SQLite。

parent 定位为侧栏 Store::open 的迁移写入与 DEFERRED commit 读后升级写冲突；WAL 快照升级会立即 BUSY，既有 5 秒 busy timeout 无法解决。生产 create_session/commit 改为 IMMEDIATE 后，本次三组完整流程均未再出现该锁退出；生产竞争回归的独立证据见 [SQLite 记录](sqlite-contention.md)。这不等于证明所有外部锁竞争均已消失。

[失败摘要](pty/failure.json) · [旧 80×24 最后真实帧](pty/failures/987052c4/80x24/workspace-flow.html) · [旧 120×40 最后真实帧](pty/failures/987052c4/120x40/workspace-flow.html)

harness 同时将五处只读 SQLite 连接改为显式 closing，避免依赖 Python GC；未用这项改动替代生产修复。退出 stderr 只放错误字段，不叠加为布局帧。首轮 Esc 与后续按键紧连造成 Alt/CSI 误解析、日志 body 读取及系统 Python 3.9 的 tomllib 依赖均已修正，这些脚本构造问题未记成生产缺陷。

## 结论边界

本地合成订阅只证明原生协议和切换，不证明真实 OAuth 登录、订阅权益或模型认证。真实 Codex 复用登录和模型任务由 root 独立记录。远端 401、设备登录失败、配置 CAS 冲突未在这个 PTY 场景中注入，相关 Rust 回归不能称为本次 PTY 实测。未实测 Linux 系统剪贴板、真实终端主题差异或长期锁定 SQLite。

## 仍可改善的具体细节

1. 80 列时会话栏占 22 列，正文余 58 列；长连接名与模型在顶栏截断，长会话名也可能前缀相同，摘要辨认仍有限。
2. API 最后 key 步骤只展示遮罩字段与导航标签，没有连接名、模型和 endpoint 的回顾摘要；提交前核对需 Tab 回看前面字段。
