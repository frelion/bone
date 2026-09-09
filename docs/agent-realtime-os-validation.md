# bone-agent 场景验证

> **历史验证记录（2026-09-08）。** 本文中的 `bone-agent` 包名、命令和测试数量描述当时的实现，不是当前 `bone-core` 验收入口；当前命令见 [bone-core handoff](bone-core-handoff.md)。

范围是进程内 bone-agent。下面区分已有自动化回归、示例入口、实现边界与延期能力；32 个场景不是 32 项已完成的端到端认证。后来落地的 headless bone-app 及整个 workspace 构建不在这份 Agent 验收结论中。

## 自动化验收入口

```sh
cargo fmt --all -- --check
cargo clippy -p bone-agent --all-targets --all-features --locked -- -D warnings
cargo test -p bone-agent --all-targets --all-features --locked
cargo test -p bone-agent --doc --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo doc -p bone-agent --no-deps --all-features --locked
cargo run -p bone-agent --example walkthrough --locked
```

2026-09-08，该轮 Agent 工作树实际通过上述全部命令：51 个单元测试通过，doctest 当时为 0，Clippy 与 rustdoc 零警告，walkthrough 输出 `1 job completed; 11 records retained`。当时同时执行了 `git diff --check`，且 `crates/bone-app` 尚未改动。这是该轮的历史验证记录，不描述后来 headless App 重写后的当前测试数量或 workspace 验证状态。

实际测试布局：

- [src/tests.rs](../crates/bone-agent/src/tests.rs)：直接给 Kernel 输入事件，检查状态、Record、Effect、Context 与读取权限。
- [src/runtime.rs](../crates/bone-agent/src/runtime.rs)：模块内 Tokio 测试，使用受控 ModelPort、ToolPort 和取消信号，检查同步 panic 的槽释放、最后一个 handle 释放清理、取消外部写后的真实 Applied 结果。当前测试不使用暂停时钟。
- [src/model.rs](../crates/bone-agent/src/model.rs)：模块内结构协议测试，覆盖 WorkProposal 往返编码及多余、缺失字段拒绝；不是连接真实 provider 的模型认证。
- [src/config.rs](../crates/bone-agent/src/config.rs)：模块内 AgentLimits 校验测试。
- [examples/walkthrough.rs](../crates/bone-agent/examples/walkthrough.rs)：唯一示例，用确定性 ModelPort 演示输入、根 Job、Outcome、观察和 shutdown；不连接真实模型或业务工具。

## 32 个场景的覆盖边界

“回归”指当前源码中有对应自动化检查；“局部回归”只覆盖所列部分；“实现边界”说明已有机制及未单独验证的部分，不等同于场景验收通过。

| 场景 | 当前覆盖方式或边界 |
| --- | --- |
| S01 简单问答 | 示例入口：walkthrough 用确定性 ModelPort 完成一次根 Job 生命周期；自然语言答案质量未做真实模型认证。 |
| S02 A 重活中启动 B 重活 | 局部回归：多个 root 的 Worker Context 隔离、目录分页；没有真实模型和工具共同负载下的并发性能验收。 |
| S03 多工作进度 | 局部回归：Job 私有记录隔离。Job/Call 状态和进度由观察接口提供；完整进度展示和隐藏推理推断不是测试结论。 |
| S04 大量等待与就绪工作 | 局部回归：Delegate 容量不足后父 Worker 收到 Audit 并继续执行。并发槽和两条 FIFO 是实现边界；未承诺按根工作或工具候选公平调度，未做负载压测。 |
| S05 单个模型失败 | 回归：同步 coordinate panic 释放槽，失败输入可重试且服从最新合并批次。所有 provider 错误与并行失败组合未穷举。 |
| S06 等待用户时来新消息 | 回归：澄清回复预留信封、Job 问题的直接回复、开放路由期间保留旧问题，以及新输入取代在途路由和失败批次合并。 |
| S07 答案交付前更正要求 | 回归：constraints 改变丢弃旧 Finish 候选和在途模型资格；新输入令旧 Coordinate 结果失效。自然语言更正的解释质量仍由模型承担。 |
| S08 B 完成，A/C 继续 | 局部回归：父工作收到 child Outcome 后重新判断，Outcome 只投递一次。更广的多个无关 root 与定时器交错没有单独场景验收。 |
| S09 修改已有目标 | 回归：依赖契约变更投递新 revision，暂停后迟到 Worker 提案不提交。迟到成功、错误、进度的全部排列没有穷举。 |
| S10 一条输入改变多个 Job | 回归：同一协调决定同时更新 owner 与 descendant 被整组拒绝，无部分应用。 |
| S11 停止期间分派迟到 | 回归：Stop 终结森林和调查树，迟到 Worker 或工具事实不复活 Job；新输入取代在途 Coordinate 后旧结果不获提交权。 |
| S12 未来工作共享要求 | 局部回归：constraints 改变撤销旧模型、Finish 与工具资格。后续投影携带当前 constraints；不证明任意自然语言约束可被硬性强制。 |
| S13 含糊目标 | 回归：调查可在所属输入路由等待期间完成；协调失效取消调查树；澄清问题有明确回复入口。 |
| S14 暂停后恢复 | 回归：Pause 丢弃旧模型提案，Resume 重新调度，两次控制变更都有 JobControlChanged；暂停同时撤销协调权。继承暂停的所有树形组合未穷举。 |
| S15 材料影响相关工作 | 回归：child Outcome 和 Inquiry Answer 的显式 evidence 定向可读，未列私有记录仍隔离；必要输入完整投影、超预算明确失败。材料语义相关性不由这些测试证明。 |
| S16 子工作与成果复用 | 回归：父子完成门禁、单次 Outcome 投递、最终 Outcome seed、显式证据授权和依赖 revision。PublishResult/Result 等待及环检测属于实现机制，尚无专门事件交错回归。 |
| S17 并发改同一文件 | 外部边界：内置工具为 read/glob/grep，没有写文件适配器验收；隔离或条件写应由具体工具实现。 |
| S18 共用浏览器 | 延期：未实现共享浏览器租约或会话协调。 |
| S19 用户同时改外部文档 | 外部边界：版本或 ETag 前置条件依赖具体适配器；当前 Kernel 不提供外部文档事务。 |
| S20 跨 Job 共享金额预算 | 延期：没有通用预算预留或核销，constraints 不是硬预算。 |
| S21 撤销权限或切换账号 | 外部边界：宿主负责账号与权限，Kernel 提供 Stop 和调用资格撤销；动态账号交接没有实现或认证。 |
| S22 工具/子模型伪造授权 | 局部回归：无效委派输入整批拒绝、精确 schema 拒绝多余字段、私有证据隔离。owner 和控制权限由 Kernel 验证；未做真实提示注入攻防认证。 |
| S23 多租户保密 | 范围边界：当前是单宿主会话内的 Job Context 隔离，没有通用多租户保密系统。 |
| S24 只读请求的数据出境 | 外部边界：数据出境由宿主与适配器控制；ReadOnly 仅表示工具效果分类，不是无泄密证明。 |
| S25 先接收“不要发送” | 实现边界：未关闭输入路由暂扣新 ExternalWrite。尚无真实发送适配器或该自然语言顺序的端到端验收。 |
| S26 先授权发送再停止 | 回归：受控 ExternalWrite 收到取消后仍可返回 Applied，Kernel 保留事实且不复活已取消 Job；父 Finish 等待已取消 child 的在途外部写结束。 |
| S27 写结果 Unknown | 实现边界：Unknown 保留在 Call 事实中，阻止后续写入及相关成功交付，宿主通过 resolve_write 确证。当前回归没有完整覆盖 Unknown 确证、旧候选和重复写入的组合。 |
| S28 重复及迟到回调 | 回归：Input 幂等，重复迟到 ToolFinished 只记录一次，暂停、Stop、批次取代后的旧模型结果不恢复资格。不是对所有回调排列的穷举。 |
| S29 进程内定时跟进 | 实现边界：Await::After、next_deadline 和 Tick 支持进程内等待；当前没有暂停时钟的专门定时交错回归，不承诺持久定时。 |
| S30 重启后的次日跟进 | 延期：没有跨重启恢复或自动重放。当前回归覆盖最后一个 handle 释放时的空闲端口清理和在途模型取消，不覆盖重启续跑。 |
| S31 输入/进度洪流 | 局部回归：普通输入容量满时保留澄清回复信封。进度去重、有界广播及 Lagged 后重取 baseline 是接口机制；尚无输入/进度洪流或慢观察者专门压测。 |
| S32 20ms 物理急停 | 不支持：Runtime 不提供硬实时物理急停保证，应由独立确定性控制系统承担。 |

## 本轮新增反例回归

当前 [Kernel 回归](../crates/bone-agent/src/tests.rs) 包含这些明确事件序列：

- `a_routing_investigation_can_finish_while_its_routing_waits`：调查 Outcome 不被它正在协助解释的用户输入挡住。
- `parent_finish_waits_for_a_cancelled_child_external_write`：child 已取消而外部写仍在途时，父不能成功完成。
- `delegate_capacity_rejection_is_returned_to_the_parent_worker`：容量拒绝进入父 Context，父可以改为本地完成。
- 在途、Failed、WaitingForUser、WaitingInquiry 和 WaitingJob 路由的批次取代回归分别检查旧结果、旧 Input ID 重试、迟到澄清、迟到 Inquiry answer 和迟到调查 Finish 都不能恢复旧解释权。
- `seed_uses_the_final_outcome_after_an_older_checkpoint`：后继工作读取最终 Outcome，而不是被旧 checkpoint 摘要覆盖。
- Outcome、Inquiry Answer 和 seed 的 evidence 回归同时检查已分享记录可读、未分享私有记录不可读。
- 子树外部写回归同时覆盖在途写结束为 Applied，以及 Unknown 在宿主 `WriteResolved` 前持续阻止父 Finish。
- 目录回归检查 16 条上限、完整 context_bytes 预算、独占游标、只投影最新页、预检去重，以及默认首页和显式末页空游标的序列化字节数。

[Runtime 回归](../crates/bone-agent/src/runtime.rs) 还检查最后一个 Agent handle 释放后的清理；[控制回归](../crates/bone-agent/src/tests.rs) 检查 Pause/Resume 的 JobControlChanged。

没有新的真实模型或外部账号认证。这些测试证明所列受控序列中的状态和协议行为，不证明模型永远正确拆分、选择证据、满足目标，也不替尚未接入的外部适配器证明权限、条件写或跨 Runtime 幂等。
