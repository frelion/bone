# Context Engine V2：当前实现说明

2026-09-10 · 已落地的简化版本。配套 [HTML](../design/context-lifecycle-concept/context-engine-v2.html)，职责总览见 [最小架构](core-simple-design.md)。

## 从一次请求理解系统

用户说“检查存储改动，补齐边界测试，先不要发布”。Core 保存原始输入，轻 Router 用 Assign 交给一个 root Worker。Worker 读原话、制定计划、调用工具；计划存在私有 Note，子任务通过 Delegate 创建。

用户之后补充要求，会形成独立输入和审阅屏障。公开历史可以先概括这条要求，但负责 Worker 必须真实接收并审阅原话，摘要不能替它确认完成。

Worker 可以否定 handoff 的理解并澄清。当前没有单独的自动换负责人动作；原子转交属于未来协议。Kernel 是确定性裁决代码，Router 和 Worker 都只能提交建议。

## 公开会话与局部工作

| 内容 | Session 公共背景 | Job 工作上下文 |
| --- | --- | --- |
| 用户 Input | 是；本次原话独立交付 | 按明确交付关系进入 |
| 公开 Reply | 是 | 按工作记录和权限进入 |
| 向用户提出的 Clarification | 是；与回答一起保留选择语境 | 按获准记录进入 |
| root Published、root Outcome | 是 | 按发布/完成/引用关系进入 |
| child Published / Outcome、私有 Note | 否 | 仅获准范围 |
| 完整工具结果 | 否 | 权威原文保留，模型可分页 |
| 当前约束、待处理输入与等待状态 | 独立必要状态 | 独立必要状态 |

Session 背景从 Core Record 构造，不由 App 决定哪些新事实成为模型记忆。正文只保存一份；视图、摘要和引用不是另一份执行真相。

SessionCheckpoint 保存 through、summary 和 evidence；Job 继续使用自己的 Checkpoint 和已读水位。Session checkpoint 作为 Record 持久化，当前 head 从已提交记录得出；Job checkpoint 仍属于对应 Job 的上下文。

## 调用时快照

Router 请求 = 原始输入批次 + 约束 + Session 背景 + 有界 root 目录/读取结果。

Worker 请求 = 本 Job 必要状态与输入 + Session 背景 + Job checkpoint / 工作记录 + 工具定义。

每次准备调用读取最新已提交公共背景。请求开始后固定，下一轮可见新公开事实。排队期间产生的事实可以进入之后的调用；不提供“只看点击提交前历史”的 W 语义。

本次原始输入或已投影 Job Record 会从公共原文 tail 去重。摘要可能提到相同事实，接受有界语义重复。Job 压缩不会把完整 Session 背景再次纳入源，私有 Job 摘要也不回灌为公共事实。

## 组合预算与选择顺序

Core 以完整序列化 DTO 的 context_bytes 为上限，不能分别证明 Session 与 Job 都能放下就直接相加。item_bytes 还限制模型生成项和显式记录页。

先尝试工具正文分页。仍超限时，根据公共背景和本地已读记录是否能实际腾出空间选择 Session 或 Job 压缩。必要输入/状态自身超限时明确失败，不能无限压历史，也不偷偷截断用户要求。

Core 字节数不是 provider token 数；适配器仍须为 instructions、协议包装、工具 schema 和输出留余量。当前没有通用精确 token 容量协议。

## 共用 compact，分别校验

CompactInput 包含 CompactScope、previous、output_bytes、through、records，统一经过 ModelPort::compact。压缩调用直接接收受限源投影，不另建会自我压缩的普通 Job。

| 规则 | Session | Job |
| --- | --- | --- |
| 来源 | Input / Reply / Clarification / root Published / root Outcome | 当前 Job 获准工作记录 |
| 前缀资格 | 公开历史完整前缀，保留最近公开记录 | 当前 revision 的有效已读前缀 |
| 基线 | 当前 Session checkpoint 的 through | Job revision 与旧 checkpoint |
| 追加新记录 | 保留新 tail，不因此拒绝固定前缀草稿 | 未读 tail 保持未读 |
| 更新结果 | 新 SessionCheckpoint Record | 本 Job checkpoint |

滚动摘要 = 旧摘要 + 新合法前缀 → 有界新摘要。输出非空、不能超预算，必须小于提交源投影；evidence 必须属于合法范围。无前缀、无收益或模型失败都明确结束本次准备，旧摘要和原文仍在。

Session 不要求所有 Job 已读才压缩。Job 的 read_through 表示有效调用接受过通知，不证明大工具正文每个字节都已阅读；剩余正文仍通过源记录回读。

## 同步按需与状态保护

当前只在调用准备超限时压缩，无后台软阈值。Session 压缩单飞，等待它的 Job 不占用普通 Worker 执行槽；其他可运行工作仍可调度。

历史覆盖 through、Job 已读水位、输入完成责任是三件事。Session 概括一个未处理请求，不能推进后两者。新用户输入仍独立影响提交门禁；摘要模型不能更新约束、执行动作或承诺任务完成。

## Durable 与重启

Runtime 在候选 Kernel 状态上处理事件，将新增 Record 与结构化快照交给 DurablePort。原子提交成功后发布对应状态并启动依赖动作；不能靠观察广播事后归档冒充提交门禁。

DurableCommit 带 commit_id、expected_revision、snapshot、records。端口负责事务和幂等，必须内部解决不确定确认；错误表示确定未提交。DurableRestore 提供快照、记录和存储 revision，快照不重复序列化原始正文。

恢复从结构化状态进行，不从自然语言摘要猜 Job 状态。旧调用资格失效，未完成执行按中断规则处理，Unknown 外部写等待核查。恢复记忆不等于重放旧网络 future。

App 提供 SQLite DurablePort、产品视图和 Workspace 写门禁。外部写的 provenance（稳定 Core `CallId` 到原始 App `RuntimeId`）与 Core 提交一起保存；新 Runtime 仍从原始账本核查 Unknown，再用稳定 CallId 回填当前 Agent。缺失 provenance 时不猜测，防止不同 Runtime 的同号 Call 被错误确认。恢复统一使用 Core 快照与记录；旧格式不迁移，缺失必需字段直接报错。

## 验证与尚未实现

已覆盖公共可见性、Session 长历史、组合超限、待审阅输入不被摘要确认、新 tail 与旧草稿、失败保留原文、恢复 checkpoint，以及既有 Job 压缩和分页测试。

尚未实现：每任务 Worker 模型选择、负责人原子转交、有界输入自动拆批、后台预压缩、摘要树、原文惰性加载和物理回收。进程仍保留原始记录；模型输入有界不表示内存有界。

滚动摘要可能遗漏或漂移。原文、来源和独立约束提高可恢复性，但协议测试不能证明语义无损；长期质量需要真实任务评测。
