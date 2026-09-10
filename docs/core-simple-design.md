# BONE：当前最小架构

2026-09-10 · 对照当前实现；本文取代旧稿中的每输入 W、后台压缩和独立 resolver 提案。

## 职责归属

```text
App：产品 Session、配置、SQLite、视图和 Workspace 写门禁
  │ 注入 ModelPort / ToolPort / DurablePort
  ▼
Core Runtime：执行 effect，提交成功后发布状态并启动依赖动作
  ▼
Core Kernel：唯一状态裁决者
  ├─ Router：模型只建议 Assign / Read / Clarify
  ├─ Worker：理解原话、计划、执行、委派、澄清
  └─ Context：内部投影和压缩函数
```

这里的 Kernel 是确定性的 Rust 状态机；Router 是它调用的轻量模型。Runtime 是执行调用与提交的宿主循环，不是一份会话记忆。Session 状态能够跨 Runtime 重启恢复，Job 是其中一项工作的局部范围。

## 模型只留 Router 与 Worker 两个主要角色

Router 将每条输入完整交给已有 root 或新 root，附短 handoff；它不写技术计划，不改约束，也不调用工具。新 root 的初始契约由 Core 构造。

Worker 始终收到原话，handoff 不能覆盖用户要求。Worker 在私有 Note 中规划，用 Delegate 创建子任务，用 ControlOwned 控制自己拥有的后代；用户 root 可以带来源输入和基版本更新约束。

Worker 可以修正 Router 的理解并向用户澄清。**当前没有独立的“反驳后原子转交负责人”动作**；这是后续候选，不能将自然语言纠错等同于完成责任转移。当前所有 Worker 共用 worker 模型配置，不增加任务分类器或 resolver。

## 一份事实，两种模型视图

| 范围 | 当前来源 | 摘要资格 |
| --- | --- | --- |
| Session | Input、Reply、向用户提出的 Clarification、user-owned root 的 Published 与 Outcome | 公开历史的完整前缀，保留最近公开记录；不等待所有 Job 已读 |
| Job | 获准的输入投递、工具结果、私有 Note、子任务结果等 | 当前 revision 中有效 Worker 已接受的已读前缀 |

事实正文只有一份；摘要是派生记忆。SessionCheckpoint 独立于 Job Checkpoint。私有 Note、child Published / Outcome 和工具正文不会自动进入 Session 背景。

每次调用组装：必要状态与原始输入 + 最新已提交 Session 背景 + 本 Job 摘要和工作记录。Router 没有最后一项。原始重复项按来源去重，摘要中的语义重叠不做复杂消除。

采用**调用时快照**：开始后请求不变，下一次调用可以看到新公开事实。没有每输入历史 W、历史 pin 或 as-of 摘要重建。公共历史追加本身不撤销已有 Worker 的提交资格；约束、revision 等相关状态仍按规则校验。

## 同一压缩协议，不同范围策略

先组装并测量整个 Core DTO，放不下先分页工具大正文，再选择合法旧前缀。Session 与 Job 都通过 ModelPort::compact，CompactInput 用 CompactScope 区分范围，携带旧摘要、源记录、through 和 output_bytes。

Session 压缩保存独立 SessionCheckpoint；Job 压缩还校验 Job revision 和已读资格。摘要必须非空、满足输出限制、比提交的源投影更小，evidence 必须属于允许来源。没有合法前缀或没有收益就明确失败，不无限重试。

压缩是调用前按需等待，没有后台软阈值、摘要树或额外 actor。Session 压缩单飞；等待它的 Job 不应耗尽工作槽。新公开尾部追加不使固定前缀草稿失效，旧摘要基线变化则不能覆盖新 head。

输入完成责任、pending review、约束和执行门禁独立于摘要。摘要提到一个请求，不代表 Worker 已读或已经完成它。原文保留，可按 Record 引用分页回查。

## Core durable 已落地

DurableCommit 原子提交新增 Record 与结构化 DurableSnapshot，使用 commit_id 和 expected_revision。DurablePort 负责幂等和事务；返回错误必须表示该提交确定未生效，确认不确定性由端口内部解决。

Runtime 在提交成功后才发布对应状态与依赖 effect。恢复传入 DurableRestore：revision、快照和原始记录；快照不重复包含所有正文。Kernel 恢复结构化状态并隔离旧调用，未完成执行按明确中断规则处理，Unknown 外部写不盲目重放。

App 实现 SQLite DurablePort 并投影产品历史；Core 是新执行状态的权威。恢复统一使用 Core 快照与记录；旧格式不迁移，缺失必需字段直接报错。Workspace 写互斥和外部核查仍属于宿主。

## 已完成与后续候选

已完成 A：轻 Router、Worker 规划与子树控制、带来源的约束提案、输入审阅屏障及失败/取消回收。

已完成 B：Core commit/restore、App SQLite 接入、提交前门禁、快照与正文分离、恢复中断语义。

已完成 C：Core 公共 Session 投影、独立 Session checkpoint、组合预算、共享 compact 协议、按需分页和压缩。

后续候选：负责人原子转交、有界输入批次、更细 provider token 容量接口、按任务选择 Worker 模型、后台预压缩、原文惰性加载和 retention。只有实际需求与评测证明必要再增加。

当前保证模型 DTO 有界；进程仍保留原始记录，不保证内存有界。摘要可能失真，原文回查和协议校验不能替代长程质量评测。
