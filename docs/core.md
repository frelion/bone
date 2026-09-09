# Core：可信 Agent 内核

`bone-core` 定义 BONE 的执行语义。它包含一个纯 Rust Kernel 和一个 Tokio Runtime：Kernel 是所有 Agent 状态的唯一写入者，Runtime 负责执行 Kernel 发出的单次调用并把结果送回。Core 不读取磁盘、网络、凭据或产品配置。

```text
Input / control
      │
      ▼
  Runtime actor ── Event ──► Kernel::step
      ▲                         │
      │                         ├── 更新 Job、Context、Call 与 Routing
      │                         ├── 追加权威 Record
      │                         └── 产生 Effect
      │                                │
      └──── ModelPort / ToolPort ◄─────┘
```

这个边界保证模型可以理解自然语言和组织工作，但不能直接改变状态。模型产出的 `KernelDecision`、`WorkProposal` 和 `CheckpointDraft` 都是建议；Kernel 会在一个提交边界内核对来源 Call、Job revision、权限、大小和当前事实，再决定是否接受。

## 状态与身份

一个 Runtime 内使用单调的 `InputId`、`JobId`、`CallId` 和 `Seq`。这些 ID 只在本 Runtime 内有意义；宿主需要持久或跨 Runtime 引用时，必须再组合自己的 Runtime / Session 身份。

`Seq` 同时是 Record 顺序和观察水位。时间等待使用 Runtime 启动后的单调时间，不把墙上时间写进状态机。Kernel 的状态主要由以下几张表组成：

- Input：原始输入、接收位置、当前 routing、必要 root Job 和最终结果。
- Routing：一次固定的输入解释批次或由 Job 发起的协调请求。
- Job：当前契约、owner、revision、局部 Context、公开 Report 和终态。
- Call：协调、工作、压缩或工具调用，以及其运行、取消、完成和外部效果事实。
- Record：所有可观察和可引用事实的权威正文。

状态转换只通过有序 Event 发生。`Kernel::step` 不等待 I/O，也不读取隐式环境。

## 输入与 Routing

`Agent::post` 接受已经由宿主分配 ID 的 `Input`。相同 ID 和相同内容重复提交是幂等的；同一 ID 对应不同内容会被拒绝。输入可以是普通新要求，也可以通过 `replying_to` / `answering` 回答当前问题；精确回答会同时核对预期的问题 Record，避免旧界面回答新的问题。

普通新输入先成为 Record。所有尚未关闭的 input routing 与这条新输入按实际接收顺序合并成新 routing，旧 routing、旧调查树和在途协调调用随即失去解释权。已经解释完、只等待必要 Job 交付的 Input 不重新 routing。这样迟到模型结果和旧 Retry 都不能覆盖更新后的用户原话。

协调模型看到的是固定输入批次、共同约束、宿主提供的只读背景、活跃 root 的有界目录和本 routing 的读取结果。它可以：

- 原子创建或更新 root Job，并声明哪些 Job 是某个 Input 的必要交付；
- 更新全局约束；
- 分页读取允许看到的 Job、目录或 Record；
- 向相关 Job 发出定向询问；
- 创建一个只读调查 Job，为当前 routing 补充材料；
- 请求用户澄清。

协调模型不能执行工具或直接写状态。整份 `Apply` 会先整体校验再提交，避免只应用一半 Job 变更。

一个 Input 的 required roots 全部进入终态后，Kernel 单独记录 `InputFinished`。Job 完成和 Input 完成是两件事：多个 Input 可以由同一 Job 承担，一个 Input 也可以等待多个 Job。

## Job 契约与拥有关系

Job 是可以独立交付的一项工作，由四部分组成：

| 部分 | 含义 |
| --- | --- |
| Spec | `goal`、`scope`、`done_when`；当前要做什么以及怎样算完成 |
| Context | 本 Job 获准读取的 Record、已读水位和可选 checkpoint |
| Report | 一份可替换的公开摘要及其显式 evidence |
| Outcome | 唯一、不可重开的终态结果 |

owner 决定权限和交付位置：

- `User` root 对用户输入负责，可以发用户回复或问题。
- `Job(parent)` 是由父 Worker 原子委派的直属 child，结果交回父 Job。
- `Routing(seq)` 只为固定输入解释批次调查；它及其子树只能使用只读工具，内部 Outcome 交回该 routing，不直接向用户交付。

Worker 的 `WorkInput` 明确携带自己的 role、是否可以提问以及可见工具，模型提交 schema 据此裁剪动作。调查角色不会看到写工具；只有 user-owned Job 会得到面向用户的提问和回复能力。Kernel 在提交边界继续核对同一权限，因此提示与 schema 的裁剪只改善模型选择，不替代授权。

Worker 每轮可以附加一条私有 Note、更新 Report、回答已经投递的 Inquiry，并选择恰好一个 `WorkStep`：

- `Continue` 继续本地推理；
- `Tool` 启动一个已注册工具；
- `Delegate` 原子创建一批直属 children；
- `Wait` 等工具、时间、child 终态或较新的公开结果；
- `AskUser`、`Reply` 完成 user-owned Job 的用户交互；
- `Inquire`、`Coordinate` 请求局部答疑或全局协调；
- `Read` 分页读取获准的目录、Job 或 Record；
- `PublishResult` 发布一个可等待的中间成果；
- `Finish` 或 `Fail` 提交终态。

委派只在父子拥有树中向下发生。子 Job 继承必要输入、明确选取的 evidence 和当前约束，不复制父 Job 的完整历史。修改父 Job 的 Spec 会提升 revision、撤销旧模型资格并取消仍活跃的拥有子树；历史成果仍可作为证据读取。

Completed Job 不会重新打开。后续工作可以创建新 Job，并用旧 Outcome summary 和显式 completion evidence 作为受限 seed。旧权限、私有 Context 和较早 checkpoint 不随 seed 继承。

## Context、Record 与证据

事实正文在 `BTreeMap<Seq, Arc<Record>>` 中只保存一份。Job Context 只持有序号队列、已读水位和 checkpoint；给模型的 DTO 是每次调用构造的有界投影，不是全局聊天快照。

Worker 默认看到：

- 当前 Spec、revision、约束、role 和该轮允许的交互能力；
- 宿主提供的只读 bootstrap background；
- checkpoint 和其后尚未处理的本地 Record；
- 当前直属 child 的卡片、待答 Inquiry 和相关 Call；
- 本角色实际可用的工具定义。

协调模型默认只看到有界的活跃 root 首页。更多目录、Job 与 Record 通过 `ReadQuery` 获取；目录每页最多 16 项，并且仍须让最终序列化后的 `CoordinateInput` 落在预算内。新查询页替换该 routing 的旧查询投影，防止翻页时不断累加。

跨 Job 信息共享采用显式能力：

- Report、Published result、Outcome 和 Inquiry answer 可列出 evidence Record。
- 接收者只能读取被明确开放的成果和 evidence；引用一个公开结果不会递归开放来源 Job 的全部工具历史或 Note。
- `ReadQuery::Record` 使用 UTF-8 byte offset 分页，返回下一偏移和是否完成；非法边界会被拒绝。
- 工具结果的权威 `ToolOutcome` 完整保存在 Record 中。自动上下文只投影在预算内的完整结果，或一个带 `source`、`offset` 和 `next_offset` 的有界页；Worker 需要剩余正文时，通过同一 Record 的下一 offset 继续读取。Record 的处理水位与页上的 `next_offset` 分别表达“已经处理这条事实”和“正文仍可继续读取”。

这种设计把审计事实与模型输入分开：保存完整结果不会迫使下一轮一次塞入完整结果，模型看到摘要也不会被当成已经看完全部证据。

## 大小预算与压缩

`AgentLimits` 同时限制容量、并发和字节数。字节预算以最终序列化后的 Core DTO 为准：

- `context_bytes`：一次 `CoordinateInput`、`WorkInput`、`CompactInput` 或完整原始模型返回的上限。模型适配器额外加入的 instructions 和 tool schema 不在其中，宿主必须给真实模型窗口保留余量。
- `item_bytes`：一条模型生成内容或一次显式 Record 页的上限。
- `tool_output_bytes`：Kernel 接受并保存的完整原始工具结果上限；这个值在工具启动时冻结。

超限模型返回替换为固定的小型 `CallError`。超限工具返回同样变成固定错误，但必须保留 `ExternalEffect`，避免把已经发生或结果未知的写入误报为没有发生。App 还会把这些 Core 限制约束到自己的持久化上限。

当普通 Worker DTO 放不下较早、已经读过的前缀时，Kernel 可以启动一次 `compact`。压缩只生成 checkpoint，不执行工具，也不与同一 Job 的有效 Worker 并行。checkpoint 覆盖明确的旧前缀，但不推进新消息的已读水位；目标或 revision 改变后到达的旧压缩结果不会成为当前记忆。

必要的新输入和投递不能靠静默截断来满足预算。无法表示关键事实时，Job 明确失败；大正文应通过 Record 分页读取。

## 调度、等待与 Inquiry

一个 Job 同时最多有一个具备提交资格的 Worker / compact Call。默认 Worker 容量是一个交互保留槽加 `background_workers` 个后台槽；routing coordination 另有自己的单调用槽，工具由 `tool_slots` 限制。等待中的 Job 不占模型槽。

每次模型调用冻结 Job revision、已见 Record 水位和已经投递的 Inquiry。以下变化会撤销旧提交资格并请求 Runtime 取消相应模型调用：

- Job pause、cancel 或 Spec revision 改变；
- 全局约束改变；
- 新输入使旧 routing 失效；
- Agent suspend、reconfigure、stop 或 shutdown。

取消本地 Future 只撤销提交权限和本地等待，不承诺 provider 停止计费。迟到模型结果只形成审计记录，不再执行其动作。

`Wait` 只引用自己能够访问的 Call 或后代 Job。等待已经完成的本 Job 工具是合法的：安装等待时 Kernel 发现条件已满足，会立即重新调度，避免“模型快照中仍在运行、提交时刚好完成”的竞态把 Job 判为失败。Job / Result 等待只沿拥有树向后代，因此结构本身保证不会形成反向等待环。

Inquiry 是定向、有限、需要结算的消息。一个 requester 对同一 target 同时最多有一个未决 Inquiry；目标可以回答、声明需要额外工作或说明当前不可回答。暂停或已终态目标立即返回明确结果。未结算 Inquiry、未读必要投递、未结束直属 child 和未知外部写都会阻止成功 Finish。

## 完成、取消与外部写

`Finish` 会在一个入口检查：

- 必要消息和查询结果已经读过；
- 已接收 Inquiry 已结算；
- 本 Job 的工具与直属 child 已结束；
- 拥有子树中没有 Running、CancelRequested 或 `ExternalEffect::Unknown` 的写入；
- user-owned Job 的当前 routing 门禁允许对用户交付。

通过后，Kernel 创建唯一 `Arc<JobOutcome>`，同时放入 Job 终态与 Record，再向仍活跃的 owner 投递 Outcome 引用。重复完成、旧完成候选和迟到结果都不能产生第二个终态。

取消沿拥有树传播，并撤销模型调用。工具调用已经发生时，其迟到结果仍要记入 Call 和 Record；尤其不能把超时或传输失败自动解释为写入未发生。结果为 `Unknown` 时，宿主在外部核查后可调用 `Agent::resolve_write`，用已知 `None` 或 `Applied` 的 `ToolOutcome` 更新同一次 Call。相同确认是幂等的，冲突确认会被拒绝，确认本身不会自动恢复已停止工作。

`stop` 终结当前工作森林并取消当前及此前输入的执行资格，但 Runtime 可以继续接收之后的新输入。`shutdown` 先 Stop，再等待本地调用清理到 grace deadline，最后返回冻结的 `final_view` 和仍未知的写入。最后一个 `Agent` handle 被丢弃时 Runtime 会自动开始关闭；需要清理报告的宿主应显式调用 `shutdown`。

## 运行中装配与观察

`Agent::suspend` 撤销模型调用并冻结新调度，同时保留 Job 图；已经开始的工具仍按启动时的端口和输出上限收尾。`Agent::reconfigure` 原子替换模型、工具和 limits，不更换 Runtime 身份或 Job 图。活跃 Agent 随即使用新配置继续；suspended Agent 只有在 `resume_scheduling` 后继续。

`Agent::observe` 返回：

- `baseline`：同一时刻的完整 `AgentView`；
- `after`：baseline 覆盖到的 Record Seq；
- `records`：该位置之后的有界 broadcast receiver。

广播只用于唤醒和低延迟观察，可能 lag。持久宿主必须在 gap 后重新取得 baseline，并用 Seq 去重归档，不能把 broadcast 当成耐久日志。`CallProgress` 是 best-effort；最终 Call 和 Tool facts 才是权威结果。

新 Runtime 可以通过 `BootstrapContext` 接收宿主选取的有界历史。它只作为只读背景出现，不导入旧 Runtime 的 ID、权限或 Record 空间；背景本身必须适配上下文预算。

## 宿主责任

Core 有意不解决以下问题：

- provider 连接、认证、重试和计费；
- 工具对真实文件系统与进程的安全隔离；
- Session、Runtime 和 Call 的跨进程稳定身份；
- Agent Record 的持久化、恢复和长期 retention；
- 外部写入的业务核查与用户审批；
- UI 呈现、焦点和交互策略。

Core 当前为保持证据引用和输入幂等，在一个 Runtime 生命周期内保留全部 records、jobs、inputs、calls 和 routings。长会话的物理回收需要先定义引用闭包与保留策略，不能按 checkpoint 水位直接删除。

## 代码入口

- [`job.rs`](../crates/bone-core/src/job.rs)：Job 契约、提案、step 和公开终态。
- [`context.rs`](../crates/bone-core/src/context.rs)：Record、证据和三种模型输入投影。
- [`kernel/`](../crates/bone-core/src/kernel/)：唯一状态机、routing、工作事务、交换和调度。
- [`runtime.rs`](../crates/bone-core/src/runtime.rs)：Tokio actor、端口调用、取消、计时和观察。
- [`model_contract.rs`](../crates/bone-core/src/model_contract.rs)：provider-neutral instructions、schema 和严格解码。

产品级持久化与生命周期见 [App](app.md)，具体模型和工具见 [Adapters](adapters.md)，验证策略见 [Testing](testing.md)。
