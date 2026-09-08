# Agent API

`bone-agent` 对宿主只提供一个运行中对象：`Agent`。它可克隆，所有副本都向同一个 Runtime Actor 发送输入或控制；Actor 内只有一个 Rust `Kernel` 写状态。

## 启动

最小示例或不需要持久化的独立宿主可以使用 `Agent::start`：

```rust,ignore
use bone_agent::{Agent, AgentLimits, ConfiguredModel};
use bone_tools::ToolLimits;

let agent = Agent::start(
    workspace,
    ConfiguredModel::new(coordinator_model, coordinator_options)?,
    ConfiguredModel::new(worker_model, worker_options)?,
    AgentLimits::default(),
    ToolLimits::default(),
)?;
```

`ConfiguredModel` 只做一件事：保证 `ModelOptions` 属于同一种 provider protocol。`Agent::start` 安装现有的 read、glob、grep 工具。它不读取保存的设置，不处理登录，也不持久化对话。BONE 产品路径由 `bone-app` 解析已保存配置、连接模型并装配历史与写工具，最终调用 `Agent::with_ports_and_background`；前端不直接构造 Agent。

自定义宿主可以实现：

```rust,ignore
use bone_agent::{Agent, AgentLimits, ModelPort, ToolPort};

let agent = Agent::with_ports(model_port, tool_ports, AgentLimits::default())?;
```

`ModelPort` 有三个有类型的方法：`coordinate`、`work`、`compact`。`ToolPort::run` 执行一次注册工具调用。每个方法只代表一次 Call，不拥有私有工作队列或另一套 Agent 生命周期。

`ModelAdapter` 和 `read_only_tools` 也从 crate 根导出。需要复用现成模型协议并加入宿主工具时，可以直接组合它们。新 Runtime 需要旧会话背景时使用 `with_ports_and_background`：

```rust,ignore
let background = BootstrapContext {
    entries: vec![BackgroundEntry::new("previous outcome", summary)],
    omitted: older_history_exists,
};
let agent = Agent::with_ports_and_background(model, tools, limits, background)?;
```

背景是宿主选出的完整只读条目，同一个 `Arc<BootstrapContext>` 会进入 `CoordinateInput` 和 `WorkInput`。它计入现有 `context_bytes`，不创建当前 Runtime 的 Input、Job、Call 或 Record。背景自身已超过预算时，Runtime 拒绝启动。

`AgentLimits` 是一个普通配置值。以 default 开始，只修改需要的字段：

```rust,ignore
let limits = AgentLimits {
    background_workers: 4,
    context_bytes: 128 * 1024,
    ..AgentLimits::default()
};
limits.validate()?;
```

Runtime 启动时会再执行同一个 `validate`，非法配置不会进入 Kernel。

## 输入

```rust,ignore
let receipt = agent
    .post(Input::new(InputId(turn), text))
    .await?;
```

`InputId` 由宿主提供，在一个 Agent 生命周期内保持稳定。相同 ID 与相同内容重投会返回原 receipt，不增加 Record；相同 ID 与不同内容会返回 `AdmissionError::ConflictingInput`。

receipt 表示输入已经被进程内 Kernel 接受，不表示工作完成或内容已经持久化。容量已满时 `post` 返回 Busy，宿主仍拥有原输入，可以稍后用同一 ID 重投。

新的普通输入会与所有未关闭输入路由中的原话按接收顺序组成新批次，包括解释失败、等待澄清、等待询问或调查的路由。旧路由及其调查树失去执行资格；原 Input 改关联到新路由，交付义务继续保留，不因批次取代而提前产生 InputFinished。已经解释完毕、仅在等待 required Job 交付的输入不重新路由。

用户回答澄清问题时，创建新 ID，并引用当前处于 WaitingForUser 的那条输入。`InputStatus::WaitingForUser` 同时返回问题文字和 `question_seq`；持久化宿主把该序号作为问题身份带回：

```rust,ignore
agent
    .post(Input::new(InputId(next_turn), answer).answering(waiting_input, question_seq))
    .await?;
```

Kernel 在接受 Input 的同一步验证 `expected_question`，所以被替换或已经回答的问题会返回 `AdmissionError::StaleReply`，不会先写入一条无效 Input。无需持久问题身份的简单调用仍可使用 `replying_to(waiting_input)`，它回答该输入当前的问题。

路由失败不会自动重复模型调用。宿主确认要重试时调用 `agent.retry(input_id).await`。Retry 只作用于该输入当前关联的失败路由；输入已合并到新批次时，旧 ID 不会恢复旧批次的决定权。迟到的旧 Coordinate 结果同样不能提交。

## 身份边界

- `InputId` 标识宿主提交的一条原话。
- `JobId` 标识一项有 owner、Spec、Context、revision 和唯一终态的语义工作。
- `CallId` 标识当前 Runtime 内的一次实际模型或工具调用。`CallContext::id()` 不保证跨 Runtime 唯一；外部工具构造幂等键时必须组合宿主提供的 Runtime 或 Session 标识。
- `Seq` 标识权威 Record 和它们的顺序。

没有 EffectId、WakeId 或 generation。Kernel 结合 Job revision、暂停状态和 active_call 判断提交资格；CallId 用于当前 Runtime 内的执行结果去重。

## 观察

```rust,ignore
let mut observation = agent.observe().await?;
render(&observation.baseline);

while let Ok(record) = observation.records.recv().await {
    if record.seq == Seq(observation.after.0 + 1) {
        render_record(&record);
        observation.after = record.seq;
    }
}
```

`observe().await` 由 Runtime Actor 原子完成两件事：先订阅后续 Record，再生成同一时刻的 `AgentView` baseline。Runtime 不在每个事件后复制完整快照。

baseline 包含全部当前 Input、Job、保留的 Record，以及仍运行、请求取消或外部效果 Unknown 的 Call。后续流只发送语义事实：输入、路由、Job 创建/改约、工作 Note/Report、工具结果、进度、Inquiry、Delivery、Outcome 和控制结果。Pause/Resume 的实际状态变更通过 `JobControlChanged` 发布。内部 Event、Effect 与调度队列不是宿主协议。

广播是有界的，慢观察者不会阻塞 Agent。收到 `Lagged` 或发现 Seq gap 后重新调用 `observe().await`，用新 baseline 恢复并替换旧 receiver。Record 可能包含用户文字和工具输出，宿主决定显示、审计及持久化策略。

宿主判断一次输入结束时应等待对应 `RecordBody::InputFinished`，或查看 `InputView.status == Finished(_)`。`Clarification` 是 WaitingForUser 边界，`InputRoutingFailed` 是可重试失败边界；两者都不是 Input 终态。一条 Reply、一个 Worker Call 完成或一个 child Outcome 都不等于输入已经结束。

## 控制与终态

```rust,ignore
assert_eq!(agent.pause(job).await?, ControlOutcome::Applied);
assert_eq!(agent.pause(job).await?, ControlOutcome::Unchanged);
agent.resume(job).await?;
agent.cancel(job).await?;
agent.stop().await?;
let report = agent.shutdown().await?;
```

`pause`、`resume`、`cancel`、`stop`、`retry` 和 `resolve_write` 都返回 `ControlOutcome::Applied` 或 `Unchanged`。回执由 Kernel 执行该控制时产生；宿主不需要先读状态再猜测调用有没有改变状态。

- Pause 保留 Job 和 Context，撤销当前调用资格；Resume 重新排队。
- Cancel 封存该 Job 及其拥有子树，迟到模型结果不能改变终态。
- Stop 取消当前森林、未完成路由和 Input，但 Runtime 仍可接受后续新输入。
- Shutdown 先执行 Stop，再等待本地 Call 清理到 grace deadline，然后关闭 Actor。

最后一个 `Agent` handle 被丢弃时，Runtime 自动执行 Stop 并进入同样的 shutdown 清理。丢弃单个 clone 不会停止其他 handle 使用的 Agent；需要取得 `ShutdownReport` 时仍应显式调用 `shutdown`。

工具调用的实际外部效果独立于 Job 终态。`ExternalEffect::Unknown` 表示调用可能已经影响远端；本地取消不能把它改成未执行。宿主取得权威结果后调用：

```rust,ignore
agent.resolve_write(call_id, verified_tool_outcome).await?;
```

Unknown 不会自动重发。`ShutdownReport::unresolved_writes` 列出关闭时仍需核对的写入，`final_view` 是 Actor 退出前冻结的最终 `AgentView`，可用于补齐有界广播中遗漏的尾部事实。

## Job 与 Context

协调模型可以建议 root Job，Worker 可以建议直属 child；模型不能填写 ID、owner 或 revision，也不能直接改变 Kernel 表。每份决定和每个 Delegate 批次都先整体校验，再一次提交。

Delegate 容量不足时一个 child 都不创建，Kernel 把拒绝原因作为 Audit 写入父 Context，并重新调度父 Worker 选择后续步骤；容量不足本身不令父 Job Failed。

Worker 每次只看到一个冻结 `WorkInput`：当前 Spec、会话约束、自己的 checkpoint 与获准 Record、直属 child 的公开卡片、交给自己的 Inquiry、自己的 Calls 和注册工具。其他并行 Job 的私有上下文不会混入。

显式 Report、Published、Outcome 和 Inquiry Answer 的 evidence 向相应接收者开放定向读取，未列出的私有记录仍隔离。Completed Job 的 seed 始终使用最终 Outcome summary，并允许读取最终 Outcome 与 evidence；checkpoint evidence 可以补充背景，旧 checkpoint 不覆盖最终结论或授予整个来源 Context 的权限。

`context_bytes` 计算序列化的 Agent context DTO，并限制一次完整原始 `ModelPort` 返回；它不计算模型适配器随后加入的 instructions 或 tool schema。原始完整返回超限会被拒绝，改写成固定的 `CallError`。`item_bytes` 限制可能进入单条 Record 的模型产物，包括 ToolCall、JobSpec、报告、完成结果、问题、回复和约束；嵌套产物超限会拒绝整份决定或提案并产生固定的小型失败/Audit，不保存超限事实。超限 CallProgress 只丢弃并写固定 Audit，不终止正在进行的调用。完整原始 `ToolOutcome` 按 `tool_output_bytes` 计量，超限时换成固定错误并保留 external-effect 判定。这些配置值只要求大于零，因此替代诊断不承诺仍小于任意荒谬的小 limit；其大小由实现固定，不随不可信 payload 增长。默认 Worker 投影只列活跃直属 child 和运行中或 Unknown 的 Call；历史事实通过 Record 与 `Read` 访问。当前 Kernel 保留本进程的完整权威历史，尚未实现物理 GC，因此这些限制不等于进程总内存上限。

Coordinator 的目录页同时受 16 条和完整 `CoordinateInput` 字节预算限制。显式 Read 查询页替换默认首页，模型投影只保留本 routing 最新查询页；旧页继续保留为权威记录。自动投影的必要事实必须完整容纳，只有显式 `ReadQuery::Record` 允许按 UTF-8 offset 分页。

`WorkProposal` 由可选 Note、可选 Report、已收到 Inquiry 的 answers，以及恰好一个 `WorkStep` 组成。Tool、Delegate、Reply 与 Finish 因而不能在同一提案里形成含糊的执行顺序。

`JobStatus::Finished` 直接携带终态 `Arc<JobOutcome>`；没有另一份可冲突的 `outcome: Option<_>`。正常 Finish 要求本 Job 的工具结束、子工作终态及必要消息已读，并等待整个拥有子树中 Running、CancelRequested 或 Unknown 的 ExternalWrite。调查及其子树的内部 Outcome 可以穿过用户交付门禁，回到所属路由；它不代表向用户完成交付。详细规则见 [Job 与 Context 设计](agent-job-context-design.md)和[实现设计](agent-job-context-implementation.md)。
