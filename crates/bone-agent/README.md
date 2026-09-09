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

需要自定义模型或工具时，实现根模块导出的 `ModelPort`、`ToolPort`，然后调用 `Agent::with_ports`。现成的 `ModelAdapter` 和 `read_only_tools` 也公开导出，宿主可以组合已有模型协议和自己的工具，无需复制适配代码。Port 的一次方法调用就是一次 Call；Port 内不再运行另一套 Agent loop。

新 Runtime 需要读取已保存历史时，宿主传入有界的只读背景：

```rust,ignore
use bone_agent::{Agent, BackgroundEntry, BootstrapContext};

let background = BootstrapContext {
    entries: vec![BackgroundEntry::new("previous outcome", summary)],
    omitted: older_history_exists,
};
let agent = Agent::with_ports_and_background(model, tools, limits, background)?;
```

同一个 `Arc<BootstrapContext>` 会出现在 `CoordinateInput` 和 `WorkInput` 中，并计入 `context_bytes`。背景本身超过预算时启动失败；其中内容只作为历史资料，不进入当前 Runtime 的 ID 和 Record 空间。

可执行的无网络示例：

```sh
cargo run -p bone-agent --example walkthrough --locked
```

## Job 生命周期

1. `post` 先把用户原话写成权威 Record。新的普通输入与所有未关闭输入路由中的原话按接收顺序合并，创建新 Routing 并撤销旧路由的解释权；旧 Input 的交付义务保留。
2. 协调模型返回一个 `KernelDecision`。它可以建议创建或更新 Job、读取公开事实、定向询问，或请求用户澄清。
3. Kernel 原子校验整份决定，再创建 Job。Worker 也可以用一个 `Delegate` step 原子创建多个直属 child；容量不足时整批拒绝，把 Audit 写入父 Context 并重新调度父 Worker，不把父 Job 判为 Failed。
4. Ready Job 获得一次冻结的 `WorkInput`。每个 Job 同时最多只有一个有提交资格的 Worker Call。
5. Worker 返回可选 Note、Report、Inquiry answers，以及恰好一个 `WorkStep`。
6. `Finish` 只有在必要消息已读、询问已结算、本 Job 工具和直属 children 已结束、整个拥有子树没有 Running、CancelRequested 或 Unknown 的 ExternalWrite 时才提交。调查及其子树的内部 Outcome 可以穿过用户交付门禁，返回所属路由。
7. `finish_job` 写入唯一 `JobOutcome`，把同一个 `Arc<JobOutcome>` 放入终态和 Record，再向仍活跃的 owner 投递引用。
8. 一个输入关联的 required roots 全部终态后，Kernel 单独写入 `InputFinished`。

模型会建议创建 Job，但不能直接创建。协调模型只能通过 `KernelDecision::Apply` 提议 root 或更新；Worker 只能通过 `WorkStep::Delegate` 提议 child。ID、owner、revision、Context 和执行资格都由 Kernel 填写。

## Context

事实正文只保存在 `BTreeMap<Seq, Arc<Record>>` 一次。每个 Job 的 Context 只保存：

- 属于它的 Record 序号队列；
- 已读水位；
- 一个可选 checkpoint。

Worker 不接收全局聊天快照。`prepare_work` 只展开本 Job 获准读取的输入、投递、工具事实和查询结果，并附带仍活跃的直属 child 卡片以及仍运行或外部效果 Unknown 的 Call。Report、Published、Outcome 和 Inquiry Answer 中显式分享的 evidence 可以定向读取；未列出的私有 Note 和工具历史仍隔离，不因引用一个 Job 而开放其全部 Context。

协调模型默认只收活跃 root 的有界首页，其余目录通过 `Read` 继续读取。查询页同时受 16 条和完整 `CoordinateInput` 的 `context_bytes` 预算限制；最新查询页替换默认首页。旧页仍保留在权威记录中，模型投影只保留本路由最新查询页，避免翻页时不断累积正文。

`AgentLimits::context_bytes` 限制序列化后的 `WorkInput`、`CoordinateInput`、`CompactInput` 以及一次完整原始 `ModelPort` 返回，不包含模型适配器随后加入的 instructions 和 tool schema；宿主仍需为真实模型窗口留出余量。完整返回超限会被拒绝并换成固定 `CallError`。可能写入单条 Record 的模型产物（例如 ToolCall、JobSpec、Report、Completion、问题和回复）另受 `item_bytes` 限制；嵌套产物超限会拒绝整份决定或提案并产生固定的小型失败/Audit，不保存超限事实。超限 CallProgress 只丢弃并写固定 Audit。自动进入 Context 的必要事实完整投影，放不下时明确失败；只有显式 `ReadQuery::Record` 按 UTF-8 offset 分页。当 Worker payload 超限时，Kernel 可以固定一个已经读过的前缀交给 `compact`。新消息留在后缀；checkpoint 不推进已读水位，也不会被再次作为普通 Record 展开。

完整序列化后的原始 `ToolOutcome` 受 `tool_output_bytes` 限制。超限结果换成固定的小错误，但保留 `external_effect`，因此不会因丢弃大正文而把已发生或未知的外部写入误报为未发生。byte limit 只要求为正数；这些替代诊断的大小由实现固定、不随不可信 payload 增长，但不承诺适配任意荒谬的小配置值。

用户问题的 `InputStatus::WaitingForUser` 同时给出文字和 `question_seq`。持久化宿主应把该序号放回回答，Kernel 会在接受 Input 的同一步核对问题仍是当前问题：

```rust,ignore
agent
    .post(Input::new(next_input, answer).answering(waiting_input, question_seq))
    .await?;
```

只需要回答当前问题的简单调用仍可使用 `replying_to(waiting_input)`。

Completed Job 可以作为新 Job 的受限 seed，旧 Job 不会重开。导入记忆始终以最终 Outcome summary 为权威，并开放该 Outcome 和最终 evidence 的定向读取；checkpoint evidence 可以补充背景，较早 checkpoint summary 不覆盖最终结论。

当前实现保证单次模型 payload、活跃 Job、并发 Call、待处理 Input/Inquiry、单项正文和工具输出的局部边界。为了保持证据引用与输入幂等，Kernel 尚未物理回收 `records/jobs/inputs/calls/routings`；完整会话历史随本次进程生命周期保留。进程内长期存储上限需要后续单独定义引用闭包与保留策略，不能简单按 checkpoint 水位删记录。

## 并发与取消

- 交互 root 有一个保留 Worker 槽；后台 Job 使用有界并发。
- 等待计时、Job、成果、询问或工具时不占模型槽。
- Pause、Cancel、Spec revision、全局 constraints 改变和 Stop 会撤销受影响的旧模型提交资格。Pause/Resume 的状态变更写入 `JobControlChanged`，观察者可以从记录流更新界面。
- 迟到模型结果只留下审计，不会复活 Job。
- 被新输入取代的旧路由及其调查树失去执行资格；旧 Coordinate 结果和 Retry 不能恢复旧解释权。
- 迟到工具结果仍然是真实执行事实；外部写入的 `Unknown` 只能由宿主通过 `resolve_write` 确证，不能自动重发。
- `pause`、`resume`、`cancel`、`stop`、`retry` 和 `resolve_write` 返回 `ControlOutcome::Applied` 或 `Unchanged`，宿主无需通过前后快照猜测控制是否生效。
- `stop` 终结当前工作森林；`shutdown` 再等待本地调用清理，并返回仍未知的外部写入以及冻结的 `final_view`。
- `suspend` 撤销模型执行并冻结新调度但保留 DAG；在途工具继续按启动快照收尾。`reconfigure` 原子替换模型、工具和限额且保留当前调度状态；活跃 Agent 立即继续，suspended Agent 只有在 `resume_scheduling` 后恢复。
- 最后一个 `Agent` handle 被丢弃时，Runtime 自动执行 Stop 并进入 shutdown 清理。需要取得清理报告的宿主应显式调用 `shutdown`。
- `CallContext::id()` 只在当前 Runtime 内唯一；外部幂等键需要组合宿主提供的 Runtime 或 Session 标识。

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
cargo fmt --all -- --check
cargo clippy -p bone-agent --all-targets --all-features --locked -- -D warnings
cargo test -p bone-agent --all-targets --all-features --locked
cargo test -p bone-agent --doc --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo doc -p bone-agent --no-deps --all-features --locked
```
