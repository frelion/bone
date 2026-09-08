# bone-agent 重写 handoff

状态：2026-09-09，Job / Context / Kernel / Runtime 重写及本轮设计审阅问题已收口。当前契约见 [实现设计](agent-job-context-implementation.md)、[crate guide](../crates/bone-agent/README.md) 和 [Agent API](agent.md)。本文件记录交接状态，不替代对应提交的验证记录。

## 范围与结构

`crates/bone-agent` 使用新的破坏性 API，没有旧兼容层。Kernel 是唯一业务状态写入者，Runtime 执行异步调用，模型返回有类型的建议。Kernel 按 routing、work、scheduler、exchange 拆分内部模块；没有新增 repository trait、事件总线包装或另一套 Agent 生命周期。

`crates/bone-app` 的 headless 重写现已落地，当前实现与契约入口见 [bone-app 架构设计](bone-app-design.md)；[旧迁移笔记](bone-app-agent-migration-plan.md) 只保留作历史 Agent API 行为参考。独立前端、TUI/CLI 和完整产品链路验证仍是后续工作，bone-agent 的收口不代表这些工作已经完成。

## 已落实的核心契约

- Job 的 owner、revision、等待、子工作及唯一 Outcome 由 Kernel 管理；每个 Job 只有一个有提交资格的 Worker Call。
- 新的普通 Input 与所有未关闭输入路由的原话按接收顺序合并，撤销旧路由、询问和调查树的资格。旧 Input 的交付义务转入新批次继续处理；旧 Coordinate 结果和 Retry 不会恢复旧解释权。
- 调查及其子树只使用只读工具。内部 Outcome 可穿过用户交付门禁，返回所属解释路由。
- 正常 Finish 等待必要消息、未决询问、本 Job 工具、子工作终态，以及整个拥有子树中 Running、CancelRequested 或 Unknown 的 ExternalWrite。取消 child 不等于它发出的外部写已经结束。
- Delegate 容量不足时整批拒绝，把 Audit 写入父 Context 并重新调度，让父 Worker 决定后续步骤；容量不足本身不会终结父 Job。
- Pause/Resume 的实际变更发布 JobControlChanged。Stop 终结当前森林；最后一个 Agent handle 释放时 Runtime 自动 Stop 并进入 shutdown 清理。
- 迟到模型结果不能提交旧提案；迟到工具结果仍记录真实效果。Unknown 由宿主确证，不自动重发。CallContext::id 仅在本 Runtime 内唯一，外部幂等键需要宿主提供的 Runtime 或 Session 标识。

## Context 与证据问题已修复

旧 handoff 列出的两个阻塞项已解决：

1. 自动投影的 Record、Delivery 及其 source 必须完整容纳，不能只显示首段后推进已读水位。必要 payload 超预算时明确返回 TooLarge；只有显式 ReadQuery::Record 允许按 UTF-8 offset 分页。压缩按固定已读前缀执行，正文去重，read_through 仅在有效 WorkProposal 提交后推进。
2. ImportedMemory 的 record_refs 形成定向读取授权，未列出的来源私有记录仍隔离。seed 始终以最终 Outcome summary 为权威，并包含最终 Outcome 与 evidence 的读取引用；checkpoint evidence 可以补充背景，旧摘要不会覆盖最终结论。

本轮同时补齐 Report、Published、Outcome 和 Inquiry Answer 的显式 evidence 读取链路。共享成果的证据引用可读取，不因此开放来源 Job 的整份私有 Context。

Coordinator 目录查询同时受 16 条和完整 CoordinateInput 的 context_bytes 预算限制。最新查询页替换默认首页，模型投影只显示本 routing 最新查询页；旧页继续保留为权威记录，翻页不累积旧页正文。

## 验证入口

以下是当前改动的验证命令，不是本文件对某次运行结果的声明；实际通过情况以对应提交的执行输出为准。

```sh
cargo fmt --all -- --check
cargo clippy -p bone-agent --all-targets --all-features --locked -- -D warnings
cargo test -p bone-agent --all-targets --all-features --locked
cargo test -p bone-agent --doc --all-features --locked
RUSTDOCFLAGS='-D warnings' cargo doc -p bone-agent --all-features --no-deps --locked
cargo run -p bone-agent --example walkthrough --locked
git diff --check
```

回归重点包括批次取代与迟到结果、调查内部完成、子树未决写、容量拒绝后重新决策、完整必要输入、显式证据隔离、最终 Outcome seed、目录多页预算、控制记录以及 handle 释放清理。不在 handoff 固定测试数量或 walkthrough 输出，避免旧结果被误当作当前完成证据。

## 独立后续工作

当前模型 payload、活跃 Job、并发调用和待处理输入等有局部容量边界；`records/jobs/inputs/calls/routings` 仍按 Runtime 生命周期保留，尚无物理 GC 或跨重启续跑。后续 retention 必须先定义输入幂等、未决写、Delivery 和 evidence 的引用保留闭包，不能直接按 checkpoint 水位删除 Record。

真实模型对拆分、证据选择、摘要和交付质量的效果评估，以及 headless bone-app 的长期运行、独立前端和完整产品链路验证，作为后续工作继续开展。
