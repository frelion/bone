# bone-agent 重写 handoff

## 当前目标

按 [`agent-job-context-implementation.md`](agent-job-context-implementation.md) 完成
`bone-agent` 的 Job / Context / Kernel / Runtime 重写。实现应保持直接、紧凑，避免兼容层、仓储
trait、事件总线包装和无实际语义的防御分支。

用户当前要求：

- 不修改 `crates/bone-app`；另一个任务会单独迁移它。
- `bone-app` 只需要参考 [`bone-app-agent-migration-plan.md`](bone-app-agent-migration-plan.md)。
- 不使用 MCP。
- `kernel.rs` 不能继续膨胀；Kernel 已拆成职责明确的内部模块。

## 已完成的实现

`crates/bone-agent` 已改成新的破坏性 API，没有保留旧兼容层：

- `job.rs`：JobSpec、Assignment、Owner、Completion、JobOutcome 和内部 Job 状态。
- `context.rs`：唯一 Record 事实、每个 Job 的 Context 索引，以及
  `prepare_work` / `prepare_coordinate` / `prepare_compact` 三个模型输入入口。
- `kernel/mod.rs`：Kernel 状态和纯 `Event -> Effect` 边界。
- `kernel/scheduler.rs`：容量、优先级和调用调度。
- `kernel/routing.rs`：Input 路由、Job 创建/更新和调查 Job。
- `kernel/work.rs`：WorkProposal 提交、等待、工具调用、完成和取消。
- `kernel/exchange.rs`：Inquiry、Read、Delivery 和读取授权。
- `runtime.rs`：唯一 actor，执行模型/工具调用并把结果送回 Kernel。
- `app.rs`：面向宿主的 `Agent` handle。
- `model.rs` / `ports.rs` / `tools.rs`：严格 DTO、端口和内置只读工具。

已经实现并测试的关键语义：

- 多个 Job 并发，但每个 worker 只得到自己的 Context 投影。
- Job 的 owner、revision、等待状态、子 Job 和终态由 Kernel 统一管理。
- 协调模型只看有界根 Job 目录和摘要；细节通过 Inquiry、Read 或调查 Job 获取。
- 同一决策不能同时修改 owner 与 descendant。
- Pause、Stop 和约束变更都会使旧模型权限失效；晚到结果不能复活 Job。
- ExternalWrite 取消后仍记录真实晚到结果，Unknown effect 可由宿主显式解析。
- 父 Job 等待子 Job 时只收到一次 Outcome delivery。
- 用户问题全局串行；澄清回答使用预留 Input envelope。
- routing failure 有带 InputId 的 `InputRoutingFailed` 事实。
- `ToolFinished` 自包含共享的 ToolCall 与 ToolOutcome。

物理保留边界已经在设计文档中如实说明：模型输入 DTO 有界，但当前进程内
`records/jobs/inputs/calls/routings` 仍按 runtime 生命周期保留；尚未实现 GC。不要在没有引用闭包设计的
情况下直接删除 Record。

## 当前验证状态

以下命令在本 handoff 写入前已经通过：

```text
cargo fmt --all --check
cargo clippy -p bone-agent --all-targets --all-features --locked -- -D warnings
cargo test -p bone-agent --all-targets --all-features --locked
RUSTDOCFLAGS='-D warnings' cargo doc -p bone-agent --all-features --no-deps --locked
cargo test -p bone-agent --doc --all-features --locked
cargo run -p bone-agent --example walkthrough --locked
git diff --check
```

当前单元测试为 30 个；walkthrough 输出：

```text
1 job completed; 11 records retained
```

这些绿灯不能作为完成证据，因为最后一次独立审阅发现了下面两个尚未修复的上下文问题。

## 必须先修的两个问题

### 1. 未读 Delivery 的 source 被截断后仍被标成已读

位置：`crates/bone-agent/src/context.rs` 的 `expand_records` / `push_view`，以及
`crates/bone-agent/src/kernel/work.rs` 的 `accept_work`。

现状：

1. Job Context 保存一个 Delivery Record 的 Seq。
2. `expand_records` 展开 Delivery 时，对 Delivery 和它的 source 都只生成最多
   `item_bytes` 的 `RecordView`。
3. source 过长时，worker 只看到首段以及 `next_offset`。
4. WorkProposal 被接受后，`seen_through` 仍推进到该 Delivery Seq，继而推进
   `read_through`。
5. Kernel 不知道 worker 是否按 `next_offset` 读完正文，所以 worker 可以在没读完整用户要求时 Finish。

第一版不应引入“部分消费 token”或新的游标状态。最小且清晰的规则是：

> 自动进入 Job Context 的未读事实是原子的；本轮必须完整投影，否则
> `prepare_work` 返回 `ContextError::TooLarge` 并明确失败。只有模型主动发起的
> `ReadQuery::Record { id, offset }` 允许按 `item_bytes` 分页。

建议把 `expand_records` 改成：

- Context 中直接附着的 Record 使用完整 `RecordView`；
- Delivery Record 及其 source 使用完整 `RecordView`；
- `ReadResult` 自身完整；只有 `ReadResult.record` 指向的 source range 使用
  `item_bytes`；
- 完整投影最后统一由 `encoded_len(input) <= context_bytes` 检查；必要事实放不下时返回
  `TooLarge`，绝不能推进 `read_through`。

需要至少增加两个回归测试：

1. `item_bytes` 很小、Input 大于 `item_bytes` 但小于 `context_bytes`：worker 得到完整 Input，
   `next_offset == None`。
2. Input 大于 `context_bytes`：不启动 Work call，Job 以明确的 context-too-large 原因失败，
   不能产生一个只看首段就完成的路径。

还应覆盖一个主动 `ReadQuery::Record`，确认它仍按 offset 分页，不因上述修复失去读取大历史事实的能力。

### 2. ImportedMemory 的 record_refs 没有形成读取授权

位置：`crates/bone-agent/src/kernel/routing.rs` 的 seed 创建逻辑，以及
`crates/bone-agent/src/kernel/exchange.rs::can_read_record`。

现状：seed 会创建：

```rust
RecordBody::ImportedMemory {
    source_job,
    source_revision,
    summary,
    record_refs,
}
```

这个 Record 会附着到新 Job，但 `can_read_record` 只沿 Delivery.source 和
ReadResult.record 授权，没有把当前 Job 已附着 ImportedMemory 的 `record_refs` 视为授权边。
因此新 Job 看得到证据 Seq，却可能读不到来源 Job 的私有 Note 或 ToolFinished。

最小修复是在 `can_read_record(job, seq)` 中检查该 Job Context 已附着的
`ImportedMemory`：若其 `record_refs` 包含 `seq`，允许读取该条 Record。授权只开放显式列出的引用，
不要开放整个来源 Job，也不要递归继承来源 Job 的所有权限。

回归测试应把现有 `a_completed_child_can_seed_its_follow_up` 扩展为：

- child 先产生一个私有 Note，并把其 Seq 放进 Completion evidence；
- follow-up 以 child 为 seed；
- follow-up 可以 `ReadQuery::Record` 读取这条 Note；
- 同一 child 的另一条、未列入 evidence 的私有 Record 仍不可读。

## 修复后完成性检查

1. 重跑上面的全部 `bone-agent` 命令。
2. 搜索旧 API、TODO 和占位实现；测试中的预期 panic 不算占位。
3. 对照实现文档逐项核对 Job 生命周期、并发容量、权限失效、Context 隔离、Read/Inquiry、
   外部写恢复、Stop/Shutdown 和公开观察协议。
4. 再让一个独立 agent 做只读的简单性与上下文审阅，特别检查：
   - 未读必要事实不会被截断后确认；
   - `read_through` 只在有效 WorkProposal 提交后推进；
   - seed 只授权显式 evidence；
   - 没有为了修复引入新的 repository trait、消费状态机或兼容抽象。
5. 保持 `git status --short crates/bone-app` 为空。

完成前不要因为现有 30 个测试为绿就宣告目标完成；补上的回归测试与独立复审也是完成条件。
