# bone-agent

**Kernel 模型管工作，主力模型做工作，代码 Kernel 维护真实状态。**

这是进程内的实时 Agent 内核。多个持续 Job 可以并存，模型调用有界并行；新消息不是后台工作完成后才能处理的“下一轮”。

当前只重写 bone-agent，**bone-app 尚未迁移到新 API**。这里不提供旧接口兼容层，也不接管配置存储、认证或数据库。

## 先跑起来看

```sh
cargo run -p bone-agent --example walkthrough --locked
cargo run -p bone-agent --example interleaving --locked
```

两个例子都执行真实 Kernel/Runtime，但使用受控模型结果，不访问模型服务。[walkthrough](examples/walkthrough.rs) 演示 A、B 并行及目标变更；[interleaving](examples/interleaving.rs) 演示异步工具、取消、定时和观察。

## 五个核心抽象

- **Event**：用户原话、调用结果、进度、定时与宿主控制的统一入口。
- **Kernel**：唯一状态写入者。`step(event) -> Vec<Effect>` 不做 I/O、不等待模型。
- **Effect**：获准启动、取消、定时或发布的执行指令，不是成功证明。
- **Runtime**：监督一次模型或工具调用，将真实结果送回 Kernel。
- **Job**：一项持续工作，保存目标、状态、成果、原始输入和关系。一次调用另记为 Call。

自然语言先交给 Kernel 模型短分派：创建、更新或控制明确的 Job。主力随后拿到原文并深入求解；工具结果直接回到所属工作，答案不再经过 Kernel 模型审批。复杂输入可以委派调查或请求澄清。

`KernelDecision` 只表达工作调整；`WorkProposal` 只表达本 Job 的材料、回复、工具和下一步。工作证据不是新用户授权：worker 发起的协调不能重写已有目标、解除暂停或改会话约束，只能在自己的工作树内行动。

## 运行规则

- 默认 1 个 Kernel 槽、1 个交互主力槽、2 个后台主力槽、8 个工具槽。新用户工作优先交互槽，也可使用空闲后台容量；后台工作不能挤占交互保留槽。
- 每 Job 至多一个有提交资格的主力。旧调用撤销资格后可以尚未结束，但仍计入实际容量。后台轮转，等待不占模型槽，工具候选按到达顺序轮转。
- 目标和控制变化只撤销相关调用；普通进度和追加材料不让全部答案重新计算。
- 拥有的子工作随父工作取消；引用关系不传播取消。等待“成果可用”和等待“整个工作完成”是两件事。
- 未解释输入暂扣新的业务写入与可能过时的旧最终答案，计算和许可内的调查继续。候选等待时排空有限输入，避免不停接收消息导致永远不能交付。
- 默认接受 32 个未解释普通输入，另保留一个定向澄清信封。Busy 没有接收消息；宿主保留并重试。Stop、显式重试和关联澄清走独立控制入口。
- 路由失败不自行重试。显式重试保留原 ID；新用户输入则与尚未解决的原话按接收顺序组成新批次，接管解释权，旧调查只能留下材料。
- Stop 同时撤销未完成分派与工作资格。已授权的外部行动只能尽力取消；本地取消不等于远端未执行。

写入最多一个结果未决的调用。`None / Applied / Unknown` 独立于 Job 生命周期，Unknown 只能通过宿主的 `resolve_write(CallId, outcome)` 确证，不自动重发。确证后相关工作重新考虑旧提案。这里没有跨工具 exactly-once、权限 DSL 或任意外部资源的事务保证；宿主与工具适配器负责实际授权、隔离及条件写。

## 接入

通过 `AgentHost` 注入两个已连接模型和执行配置，或直接用 `Runtime::spawn` 注入受控端口。完整入口见 [Agent API](../../docs/agent.md)。

`post(Input::new(InputId(1), text))` 返回接受收据。同 ID 同内容重投幂等；改内容会拒绝。`Input::replying_to` 可澄清或纠正未决输入，也可回答工作正在等待的问题。

要求主力处理的新输入会撤销旧调用的交付资格，即使目标文字没变；所有原话按实际接收顺序提供。Keep 保留已有暂停状态，只有用户触发的 Resume 才能恢复暂停工作。

输入已处理、Job 完成、整个请求交付、Runtime 关闭有独立通知。一个输入可能关联多个交付 Job；宿主应等待对应 `InputFinished`，不能凭一条 Reply 或某个 Call 结束判断请求完成。

`observe()` 原子返回快照、序号和后续事件。慢观察者不阻塞执行；收到 Lagged 后重新 observe。事件与快照包含用户和工具材料，宿主负责隐私与保存策略；它们不是跨重启恢复协议。

## 阅读与验证

从 [lib.rs](src/lib.rs) 的事件和效果读起，然后看 [ports.rs](src/ports.rs) 的协议、[kernel.rs](src/kernel.rs) 的 `step / advance`、[runtime.rs](src/runtime.rs) 的执行循环。模型提示及协议在 [model.rs](src/model.rs)，角色上下文在 [context.rs](src/context.rs)。

```sh
cargo fmt -p bone-agent --check
cargo clippy -p bone-agent --all-targets --all-features --locked -- -D warnings
cargo test -p bone-agent --all-targets --all-features --locked
cargo test -p bone-agent --doc --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo doc -p bone-agent --no-deps --all-features --locked
```

[设计](../../docs/agent-realtime-os-design.md)记录取舍，[场景验证](../../docs/agent-realtime-os-validation.md)区分自动化测试与明确不覆盖的场景。首版不承诺持久续跑、浏览器共享资源管理、多租户隔离或硬实时控制。
