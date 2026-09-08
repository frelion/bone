# bone-agent

`bone-agent` 是一个进程内的实时 Agent OS。多个 Job 可以同时存在；Rust Kernel 是唯一状态写入者，模型只提交有类型的建议，Runtime 只执行 Kernel 已批准的调用。

```text
                        typed result
  host ──> Agent ──> Runtime Actor ───────────────┐
              │          │                         │
              │          ├── ModelPort             │
              │          └── ToolPort              │
              │                                    ▼
              └── observe <── Record <── Kernel::step
                                           │
                                           ├── Job forest
                                           ├── scoped Context
                                           ├── Routing / Inquiry
                                           └── Call truth
```

协调模型属于 Agent Kernel 的决策路径，但它不是状态内核本身。代码 Kernel 决定何时需要协调，Runtime 执行这次模型调用，模型返回 `KernelDecision`，代码 Kernel 完整校验后才改变 Job。这样模型可以理解自然语言，Rust 仍然掌握并发、权限、生命周期和终态。

## 最小入口

```rust,ignore
use bone_agent::{Agent, AgentLimits, ConfiguredModel, Input, InputId};
use bone_tools::ToolLimits;

let agent = Agent::start(
    workspace,
    ConfiguredModel::without_options(coordinator_model),
    ConfiguredModel::without_options(worker_model),
    AgentLimits::default(),
    ToolLimits::default(),
)?;

let receipt = agent
    .post(Input::new(InputId(1), "investigate the failing build"))
    .await?;
let observation = agent.observe().await?;
```

需要自定义模型或工具时，实现根模块导出的 `ModelPort`、`ToolPort`，然后调用 `Agent::with_ports`。Port 的一次方法调用就是一次 Call；Port 内不再运行另一套 Agent loop。

可执行的无网络示例：

```sh
cargo run -p bone-agent --example walkthrough --locked
```

## Job 生命周期

1. `post` 先把用户原话写成权威 Record，并创建一个 Routing。
2. 协调模型返回一个 `KernelDecision`。它可以建议创建或更新 Job、读取公开事实、定向询问，或请求用户澄清。
3. Kernel 原子校验整份决定，再创建 Job。Worker 也可以用一个 `Delegate` step 原子创建多个直属 child。
4. Ready Job 获得一次冻结的 `WorkInput`。每个 Job 同时最多只有一个有提交资格的 Worker Call。
5. Worker 返回可选 Note、Report、Inquiry answers，以及恰好一个 `WorkStep`。
6. `Finish` 只有在必要消息已读、询问已结算、工具和直属 children 已结束、整个子树没有未知写入时才提交。
7. `finish_job` 写入唯一 `JobOutcome`，把同一个 `Arc<JobOutcome>` 放入终态和 Record，再向仍活跃的 owner 投递引用。
8. 一个输入关联的 required roots 全部终态后，Kernel 单独写入 `InputFinished`。

模型会建议创建 Job，但不能直接创建。协调模型只能通过 `KernelDecision::Apply` 提议 root 或更新；Worker 只能通过 `WorkStep::Delegate` 提议 child。ID、owner、revision、Context 和执行资格都由 Kernel 填写。

## Context

事实正文只保存在 `BTreeMap<Seq, Arc<Record>>` 一次。每个 Job 的 Context 只保存：

- 属于它的 Record 序号队列；
- 已读水位；
- 一个可选 checkpoint。

Worker 不接收全局聊天快照。`prepare_work` 只展开本 Job 获准读取的输入、投递、工具事实和查询结果，并附带仍活跃的直属 child 卡片以及仍运行或外部效果 Unknown 的 Call。其他 Job 的私有 Note、工具输出和中间推理不会进入它的输入。协调模型默认只收有界的活跃 root 页，其余目录通过 `Read` 继续读取。

`AgentLimits::context_bytes` 限制序列化后的 `WorkInput`、`CoordinateInput` 或 `CompactInput`，不包含模型适配器随后加入的 instructions、tool schema 和输出预留；宿主需要为真实模型窗口留出余量。当 Worker payload 超限时，Kernel 固定一个已经读过的前缀交给 `compact`。新消息留在后缀；checkpoint 不推进已读水位，也不会被再次作为普通 Record 展开。完成后的 Job 只能作为新 Job 的受限 seed 使用，旧 Job 不会重开。

当前实现保证单次模型 payload、活跃 Job、并发 Call、待处理 Input/Inquiry、单项正文和工具输出的局部边界。为了保持证据引用与输入幂等，Kernel 尚未物理回收 `records/jobs/inputs/calls/routings`；完整会话历史随本次进程生命周期保留。进程内长期存储上限需要后续单独定义引用闭包与保留策略，不能简单按 checkpoint 水位删记录。

## 并发与取消

- 交互 root 有一个保留 Worker 槽；后台 Job 使用有界并发。
- 等待计时、Job、成果、询问或工具时不占模型槽。
- Pause、Cancel、Spec revision、全局 constraints 改变和 Stop 会撤销受影响的旧模型提交资格。
- 迟到模型结果只留下审计，不会复活 Job。
- 迟到工具结果仍然是真实执行事实；外部写入的 `Unknown` 只能由宿主通过 `resolve_write` 确证，不能自动重发。
- `stop` 终结当前工作森林；`shutdown` 再等待本地调用清理，并返回仍未知的外部写入。

## 代码阅读顺序

1. [`job.rs`](src/job.rs)：Job 契约、proposal、step、公开终态。
2. [`context.rs`](src/context.rs)：权威 Record 和三种纯上下文投影。
3. [`kernel/mod.rs`](src/kernel/mod.rs)：状态词汇与唯一事件入口。
4. [`kernel/scheduler.rs`](src/kernel/scheduler.rs)、[`work.rs`](src/kernel/work.rs)、[`routing.rs`](src/kernel/routing.rs)、[`exchange.rs`](src/kernel/exchange.rs)：调度、工作事务、协调和消息交换。
5. [`runtime.rs`](src/runtime.rs)：一个 Tokio Actor，只负责调用、取消、时间和观察。
6. [`model.rs`](src/model.rs)：协调、工作和压缩的精确结构化协议。

整体理由见 [Job 与 Context 设计](../../docs/agent-job-context-design.md)，代码不变量见 [实现设计](../../docs/agent-job-context-implementation.md)，宿主 API 见 [Agent API](../../docs/agent.md)。

## 验证

```sh
cargo fmt --all --check
cargo clippy -p bone-agent --all-targets --all-features --locked -- -D warnings
cargo test -p bone-agent --all-targets --all-features --locked
cargo test -p bone-agent --doc --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo doc -p bone-agent --no-deps --all-features --locked
```
