# Core：可信 Agent 内核

`bone-core` 定义 BONE 的执行语义。它包含一个纯 Rust Kernel 和一个 Tokio Runtime：Kernel 是所有 Agent 状态的唯一写入者，Runtime 负责执行 Kernel 发出的调用并通过注入的 DurablePort 提交状态。Kernel 不执行 I/O；模型、工具与存储的具体实现由宿主提供。

```text
Input / control
      │
      ▼
  Runtime actor ── Event ──► Kernel::step
      ▲                         │
      │                         ├── 更新 Conversation、Job、Context 与 Call
      │                         ├── 追加权威 Record
      │                         └── 产生 Effect
      │                                │
      └──── ModelPort / ToolPort ◄─────┘
```

这个边界保证模型可以理解自然语言和组织工作，但不能直接改变状态。模型产出的 `ConversationStep`、`WorkProposal` 和 `CheckpointDraft` 都是建议；Kernel 会在一个提交边界内核对来源 Call、Job revision、权限、大小和当前事实，再决定是否接受。

## 状态与身份

一个 Core 状态空间内使用单调的 `InputId`、`JobId`、`CallId` 和 `Seq`。恢复保留该状态空间的身份与记录引用；跨产品会话引用时仍须组合 Session 身份。

`Seq` 同时是 Record 顺序和观察水位。时间等待使用 Runtime 启动后的单调时间，不把墙上时间写进状态机。Kernel 的状态主要由以下几张表组成：

- Input：原始输入、接收位置、是否已处置、必要 root Job 和显式结案结果。
- Conversation：唯一持续会话状态、当前调用、上下文读取位置和待回答问题。
- Job：当前契约、owner、固定工具授权、revision、局部 Context、公开 Report 和终态。
- Call：协调、工作、压缩或工具调用，以及其运行、取消、完成和外部效果事实。
- Record：所有可观察和可引用事实的权威正文。

状态转换只通过有序 Event 发生。`Kernel::step` 不等待 I/O，也不读取隐式环境。

## 持续会话与输入

`Agent::post` 接受宿主分配 ID 的 `Input`。相同 ID 和相同内容重复提交是幂等的；冲突内容被拒绝。普通消息和澄清回答都先进入同一份 Conversation；精确回答核对问题 Record，不能恢复过期问题对应的任务。

Kernel model 使用 `ModelPort::converse` 处理会话。它看到未结案输入、连续公共历史、工具目录、有界 Job 目录和读取结果，可以直接回答或提问，也可以创建 Job、向已有 Job 发送补充、控制任务、更新约束、读取记录和等待。它没有外部工具调用动作；目录用于选择创建权限，所有外部执行都属于 Job。

问候直接产生会话回复，不创建通用 root Job。新用户输入使旧会话提案失效，但不自动取消无关工作。会话决定输入是在追问、修正还是提出新任务；向相关 Job 发送要求时撤销该 Job 的旧模型提交资格。

只有会话产生用户 `Reply` 与 `Clarification`。回复可为阶段说明，也可以通过明确 outcome 结案指定输入；相关必要工作尚未结束时不能结案。Job 结束只保存结果并唤醒会话，不自动写 `InputFinished`。Headless 等待明确结案，不能根据空闲或单个 Job 结束判断成功。

会话通知只保留尚未消费的 Record 序号，公共历史由现有 Session background 和压缩负责。调用期间新到达的任务结果不会被旧调用的完成覆盖；正在等待用户回答时，后台结果不触发重复提问。无效动作记录 `ConversationRejected` 并有限纠错，达到预算后停下并报告失败；新输入或明确重试重置纠错预算。

## Job 契约与拥有关系

Job 是可以独立交付的一项工作，包含：

| 部分 | 含义 |
| --- | --- |
| Spec | `goal`、`scope`、`done_when`；当前要做什么以及怎样算完成 |
| Allowed tools | 创建时确定的工具名称集合；修改契约不会更改授权 |
| Context | 本 Job 获准读取的 Record、已读水位和可选 checkpoint |
| Report | 一份可替换的公开摘要及其显式 evidence |
| Outcome | 唯一、不可重开的终态结果 |

owner 决定权限和交付位置：

- `User` root 由会话创建，结果交回会话，不直接发布用户回复。
- `Job(parent)` 是由父 Worker 原子委派的直属 child，结果交回父 Job。

Worker 的 `WorkInput` 携带执行契约、上下文和获准工具。所有 Worker 使用同一种执行协议，没有用户对话角色。需要信息时通过 `NeedInput` 向 owner 报告问题并等待；父 Job 可用 `Respond` 回答直属 child，只有会话向用户提问。回答必须指明仍在等待的具体问题 Record。Kernel 在提交边界继续核对权限，模型可见列表不替代授权。

### Job 工具授权

会话创建和 Worker 委派都使用 `Assignment.tools: Option<ToolSelection>`。省略时，新 root 获得当前注册工具集合，child 复制父 Job 的集合；`ReadOnly` 按工具注册时的 `ToolEffect::ReadOnly` 筛选；`Only(names)` 请求具体工具。未知工具或超出父授权的请求被拒绝，空集合合法。Job 创建后只保存解析后的 `allowed_tools`，不保存动态继承或全选表达式。

会话模型负责理解“只读分析”“不要运行命令”“只使用 read 和 grep”等自然语言，Kernel 负责解析和执行结构化授权。明确限制与现有 Job 不匹配时必须建立新 Job，不能修改旧 Job 权限。普通追加输入和澄清回答沿用既有权限。自然语言理解可能遗漏限制，界面应展示实际授权，不能把模型理解本身当作强制保证。

构造 `WorkInput.tools` 时按 Job 集合过滤目录；`validate_proposal` 在产生工具执行 effect 前强制检查成员资格。工具实例、runtime 和 app 不重复实现授权。权限不随任务修改、配置更新或新增工具扩大。恢复保留授权事实，但不改变现有中断未完成 Job 的语义。

工具权限不是数据隔离或 OS 沙箱：内核 `Read`、`Delegate` 等动作遵循各自的拥有关系；允许 `bash` 可能读写文件。`ToolEffect` 和写入 gate 继续负责外部副作用的顺序与恢复。

Worker 每轮可以附加一条私有 Note、更新 Report、回答已经投递的 Inquiry，并选择恰好一个 `WorkStep`：

- `Continue` 继续本地推理；
- `Tool` 启动一个已注册工具；
- `Delegate` 原子创建一批直属 children；
- `Wait` 等工具、时间、child 终态或较新的公开结果；
- `NeedInput` 向 owner 报告缺少的信息并等待；
- `Respond` 回答直属 child 的确切待答问题；
- `Inquire` 向可访问的 Job 请求局部答疑；
- `ControlOwned` 暂停、恢复或取消自己拥有树中的活跃后代；
- `Read` 分页读取获准的目录、Job 或 Record；
- `PublishResult` 发布一个可等待的中间成果；
- `Finish` 或 `Fail` 提交终态。

委派只在父子拥有树中向下发生。子 Job 继承必要输入、明确选取的 evidence 和当前约束，不复制父 Job 的完整历史。Worker 的普通计划写入 Note，并通过后续步骤和 Delegate 落地，不修改正式 root 契约。

Completed Job 不会重新打开。后续工作可以创建新 Job，并用旧 Outcome summary 和显式 completion evidence 作为受限 seed。旧权限、私有 Context 和较早 checkpoint 不随 seed 继承。

## Context、Record 与证据

事实正文在 `BTreeMap<Seq, Arc<Record>>` 中只保存一份。Job Context 只持有序号队列、已读水位和 checkpoint；给模型的 DTO 是每次调用构造的有界投影，不是全局聊天快照。

Worker 默认看到：

- 当前 Spec、revision、约束和固定工具权限；
- Core 构造的 Session 公共摘要与原文尾部（Input、Reply、用户 Clarification、user-owned root 的 Published / Outcome / JobNeedsInput）；
- checkpoint 和其后尚未处理的本地 Record；
- 当前直属 child 的卡片、待答 Inquiry 和相关 Call；
- 本 Job 实际可用的工具定义。

会话模型看到连续公共历史及有界 Job 目录。会话读取和向新任务交付证据使用同一个记录权限判断，模型协议仅列出实际可见的证据来源。更多 Job 与 Record 通过 `ReadQuery` 获取；分页仍须满足 `ConversationInput` 的序列化预算。读取游标只说明看到了哪些事实，不表示已处置用户输入，也不解除外部写入屏障。

跨 Job 信息共享采用显式能力：

- Report、Published result、Outcome 和 Inquiry answer 可列出 evidence Record。
- 接收者只能读取被明确开放的成果和 evidence；引用一个公开结果不会递归开放来源 Job 的全部工具历史或 Note。
- `ReadQuery::Record` 使用 UTF-8 byte offset 分页，返回下一偏移和是否完成；非法边界会被拒绝。
- 工具结果的权威 `ToolOutcome` 完整保存在 Record 中。自动上下文只投影在预算内的完整结果，或一个带 `source`、`offset` 和 `next_offset` 的有界页；Worker 需要剩余正文时，通过同一 Record 的下一 offset 继续读取。Record 的处理水位与页上的 `next_offset` 分别表达“已经处理这条事实”和“正文仍可继续读取”。

这种设计把审计事实与模型输入分开：保存完整结果不会迫使下一轮一次塞入完整结果，模型看到摘要也不会被当成已经看完全部证据。

## 大小预算与压缩

`AgentLimits` 同时限制容量、并发和字节数。字节预算以最终序列化后的 Core DTO 为准：

- `context_bytes`：一次 `ConversationInput`、`WorkInput`、`CompactInput` 或完整原始模型返回的上限。模型适配器额外加入的 instructions 和 tool schema 不在其中，宿主必须给真实模型窗口保留余量。
- `item_bytes`：一条模型生成内容或一次显式 Record 页的上限。
- `tool_output_bytes`：Kernel 接受并保存的完整原始工具结果上限；这个值在工具启动时冻结。

超限模型返回替换为固定的小型 `CallError`。超限工具返回同样变成固定错误，但必须保留 `ExternalEffect`，避免把已经发生或结果未知的写入误报为没有发生。App 还会把这些 Core 限制约束到自己的持久化上限。

Conversation 或 Worker 的组合 DTO 超限时，Kernel 先尝试分页工具正文，再按实际占用选择 Session 或 Job 的合法前缀，通过 `CompactScope` 共用 `ModelPort::compact`。Session 来源仅为 Input、Reply、用户 Clarification 和 user-owned root 的 Published / Outcome / JobNeedsInput；SessionCheckpoint 独立于 Job checkpoint，保留最近公开记录，不需要全体 Job 已读。Job 压缩只生成 checkpoint，不执行工具，也不与同一 Job 的有效 Worker 并行。checkpoint 覆盖明确的旧前缀，但不推进新消息的已读水位；目标或 revision 改变后到达的旧压缩结果不会成为当前记忆。

Session 和 Job 的草稿都必须非空、满足输出预算并有压缩收益；无合法前缀或无收益时明确失败。Session 新尾部追加不使固定前缀失效，摘要覆盖不确认输入已处理。当前采用同步按需压缩，没有后台阈值策略。

暂停或重配置撤销 Session 压缩调用后，迟到的成功或失败结果只结束调用记账，不提交摘要、不终结请求者；恢复调度时重新准备上下文。

必要的新输入和投递不能靠静默截断来满足预算。无法表示关键事实时，Job 明确失败；大正文应通过 Record 分页读取。

## 调度、等待与 Inquiry

一个 Job 同时最多有一个具备提交资格的 Worker / compact Call。默认 Worker 容量是一个交互保留槽加 `background_workers` 个后台槽；Conversation 另有自己的单调用槽，工具由 `tool_slots` 限制。等待中的 Job 不占模型槽。

每次模型调用冻结 Job revision、已见 Record 水位和已经投递的 Inquiry。以下变化会撤销旧提交资格并请求 Runtime 取消相应模型调用：

- Job pause 或 cancel；
- 全局约束改变；
- 新输入使旧会话提案失效；
- Agent suspend、reconfigure、stop 或 shutdown。

取消本地 Future 只撤销提交权限和本地等待，不承诺 provider 停止计费。迟到模型结果只形成审计记录，不再执行其动作。

新 Input 从接收开始阻止新的外部写提交。Conversation 必须通过创建、发送或结案等有效动作明确处置它；读取、翻页或阶段回复不能仅因推进读取游标就放行。向已有 Job 发送补充后，旧模型提案失效；已经开始的外部调用按真实结果收尾，不能假装撤回。

`Wait` 只引用自己能够访问的 Call 或后代 Job。等待已经完成的本 Job 工具是合法的：安装等待时 Kernel 发现条件已满足，会立即重新调度，避免“模型快照中仍在运行、提交时刚好完成”的竞态把 Job 判为失败。Job / Result 等待只沿拥有树向后代，因此结构本身保证不会形成反向等待环。

Inquiry 是定向、有限、需要结算的消息。一个 requester 对同一 target 同时最多有一个未决 Inquiry；目标可以回答、声明需要额外工作或说明当前不可回答。暂停或已终态目标立即返回明确结果。未结算 Inquiry、未读必要投递、未结束直属 child 和未知外部写都会阻止成功 Finish。

## 完成、取消与外部写

`Finish` 会在一个入口检查：

- 必要消息和查询结果已经读过；
- 已接收 Inquiry 已结算；
- 本 Job 的工具与直属 child 已结束；
- 拥有子树中没有 Running、CancelRequested 或 `ExternalEffect::Unknown` 的写入；
- 新输入的处置屏障允许提交当前完成提案。

通过后，Kernel 创建唯一 `Arc<JobOutcome>`，同时放入 Job 终态与 Record，再向仍活跃的 owner 投递 Outcome 引用。重复完成、旧完成候选和迟到结果都不能产生第二个终态。

取消沿拥有树传播，并撤销模型调用。工具调用已经发生时，其迟到结果仍要记入 Call 和 Record；尤其不能把超时或传输失败自动解释为写入未发生。结果为 `Unknown` 时，宿主在外部核查后可调用 `Agent::resolve_write`，用已知 `None` 或 `Applied` 的 `ToolOutcome` 更新同一次 Call。相同确认是幂等的，冲突确认会被拒绝，确认本身不会自动恢复已停止工作。

`stop` 终结当前工作森林并取消当前及此前输入的执行资格，但 Runtime 可以继续接收之后的新输入。`shutdown` 先 Stop，再等待本地调用清理到 grace deadline，最后返回冻结的 `final_view` 和仍未知的写入。最后一个 `Agent` handle 被丢弃时 Runtime 会自动开始关闭；需要清理报告的宿主应显式调用 `shutdown`。

## 运行中装配与观察

`Agent::suspend` 撤销模型调用并冻结新调度，同时保留 Job 图；已经开始的工具仍按启动时的端口和输出上限收尾。`Agent::reconfigure` 原子替换模型、工具和 limits，不更换 Runtime 身份或 Job 图。活跃 Agent 随即使用新配置继续；suspended Agent 只有在 `resume_scheduling` 后继续。

`Agent::observe` 返回：

- `baseline`：同一时刻的完整 `AgentView`；
- `after`：baseline 覆盖到的 Record Seq；
- `records`：该位置之后的有界 broadcast receiver。

广播只用于唤醒和低延迟观察，可能 lag。视图消费者必须在 gap 后重新取得 baseline，并用 Seq 去重，不能把 broadcast 当成耐久日志；持久确认由 DurablePort 完成。`CallProgress` 是 best-effort；最终 Call 和 Tool facts 才是权威结果。

每次调用使用最新已提交的 Session 公共投影，开始后请求固定；没有输入接受时的历史 W。恢复统一读取 Core 的 durable 状态。

## Durable 提交与恢复

`DurablePort::commit` 原子保存 `DurableCommit` 中的新增 Record 与结构化 `DurableSnapshot`，使用 commit_id 幂等和 expected_revision 冲突检查。端口内部必须处理确认不确定性；返回错误表示确定没有应用提交。

Runtime 提交成功后才发布相应状态和依赖 effect。恢复使用 `DurableRestore` 的 revision、snapshot 和 records；快照不重复包含所有原始正文。Kernel 从结构化状态恢复输入责任、权限、约束与上下文，撤销旧调用资格，未完成 Job 和输入明确记为失败，清除旧待答问题；新输入可引用历史，但不会重新授权旧执行。Unknown 外部写不自动重放。

宿主取消会立即发出执行取消信号。关机即使正在等待 durable ACK，也受 shutdown grace 约束；到期时停止 actor，只返回最后确认的状态，并通过 `ShutdownReport.pending_commit` 与 `AgentError::CommitInterrupted` 明确标记未确认提交。丢弃等待 future 不证明事务失败；宿主必须保护仍在进行的存储事务，待其结束后重新读取持久状态。

App 实现 SQLite 事务并投影产品视图；新执行状态以 Core 快照与记录为权威。外部写还把稳定的 Core `CallId` 与创建调用的原始 App `RuntimeId` 随提交原子保存。恢复到新 Runtime 后，App 从原始写账本核查 Unknown，再把结果提交给当前 Agent 中保留的同一 Core Call；缺失 provenance 时不猜测 Runtime，避免同号 Call 误判。状态格式和来源字段必须满足当前协议；不推断旧状态、不从产品历史重建执行上下文。

Core 状态通过同一事务中的 manifest 与有界分段保存；单文档 8 MiB 限制不再成为会话累计历史上限。分段读取使用同一个 SQLite 快照，避免读到不同提交的片段。当前仍重写完整状态，分段解决容量边界，增量写入和长期回收属于后续优化。

App 使用独立、持久的 `core_through` 记录产品投影进度，只有归档记录与更新输入视图的事务成功后才推进。Core 提交时同时保存记录来源 Runtime；恢复发生在 App 登记新 Runtime 之前，即使再次中断，也可以按真实来源补齐尚未投影的记录。

## 宿主责任

Core 有意不解决以下问题：

- provider 连接、认证、重试和计费；
- 工具对真实文件系统与进程的安全隔离；
- 产品 Session 身份及跨 Workspace 的资源协调；
- DurablePort 的具体存储实现和长期 retention；
- 外部写入的业务核查与用户审批；
- UI 呈现、焦点和交互策略。

Core 当前为保持证据引用和输入幂等，在一个 Runtime 生命周期内保留全部 records、jobs、inputs 和 calls。长会话的物理回收需要先定义引用闭包与保留策略，不能按 checkpoint 水位直接删除。

## 代码入口

- [`job.rs`](../crates/bone-core/src/job.rs)：Job 契约、提案、step 和公开终态。
- [`context.rs`](../crates/bone-core/src/context.rs)：Record、证据和三种模型输入投影。
- [`kernel/`](../crates/bone-core/src/kernel/)：唯一状态机、持续会话、工作事务、交换和调度。
- [`runtime.rs`](../crates/bone-core/src/runtime.rs)：Tokio actor、端口调用、取消、计时和观察。
- [`model_contract.rs`](../crates/bone-core/src/model_contract.rs)：provider-neutral instructions、schema 和严格解码。

产品级持久化与生命周期见 [App](app.md)，具体模型和工具见 [Adapters](adapters.md)，验证策略见 [Testing](testing.md)。
