# bone-agent：Job 与上下文的代码实现设计

状态：2026-09-08，核心实现已落地。本文件说明当前代码边界与不变量；旧 API 已直接移除，没有兼容层。模型投影已有边界，权威历史的物理 retention 留作独立后续阶段。

## 1. 实现只保留三层

```text
Agent                       宿主入口与观察
    │
Runtime                     一个 Tokio 事件循环，执行所有异步调用
    │
Kernel                      一个普通 Rust 对象，唯一业务状态所有者
    ├── Job                 工作契约、调度状态、局部上下文索引
    ├── Record              原话、材料、报告、成果的唯一正文
    ├── Call                实际调用及外部效果
    └── Routing / Inquiry   尚未完成的协调请求与定向询问
```

Kernel 不 await、不读系统时钟、不做文件或网络 I/O。Runtime 不决定工作目标、不整理另一份 Job 状态。只有 `ModelPort` 和 `ToolPort` 两个执行边界需要 trait；内部用具体类型、普通函数和标准容器。

协调模型是内核的推理角色：Kernel 决定何时请求它、给它哪些上下文，并验证和应用它的决策。模型连接由 Runtime 持有并执行异步请求，这不把调度和协调职责交给 Runtime。

不引入 Job actor、ContextService、JobRepository、CommandBus、通用事务计划或工作流节点。`Spec / Context / Report / Outcome` 是数据边界，不各自配一个管理器。

以下 Rust 代码固定核心形状，省略 derive、错误展示及完整观察 DTO；辅助类型在相应段落说明，不作为可直接编译的补丁。

## 2. 文件职责

| 文件 | 内容 |
| --- | --- |
| `lib.rs` | 小的公开入口与 re-export |
| `job.rs` | Job 契约、提案、报告、终态、内部 Job 状态 |
| `context.rs` | Record、局部索引、冻结输入构造、预算、checkpoint |
| `kernel/mod.rs` | 状态词汇、唯一事件入口、视图和 Record 写入 |
| `kernel/scheduler.rs` | ready 队列、模型/工具 Call 生命周期 |
| `kernel/work.rs` | proposal 事务、等待、Job 完成与控制 |
| `kernel/routing.rs` | 协调决定、Job 创建和路由收尾 |
| `kernel/exchange.rs` | inquiry、read、delivery 和访问边界 |
| `runtime.rs` | Agent、单一 Actor、future 监督、取消、超时、观察 |
| `ports.rs` | Event / Effect / Call、模型和工具端口 |
| `model.rs` | 两个已连接模型、提示词、结构化协议的编码与解析 |
| `config.rs` | 一份经过验证的普通 AgentLimits |
| `app.rs`、`tools.rs` | 宿主模型注入与现有工具适配 |

五个 kernel 文件共同实现同一个 `Kernel`；拆分只提供阅读边界，没有增加管理器对象或 trait。

## 3. 身份与时间

```rust
struct InputId(u64); // 宿主提供，同 ID 同内容幂等
struct JobId(u64);   // Kernel 分配，一项交付责任
struct CallId(u64);  // 一次真实模型或工具调用
struct Seq(u64);     // Kernel 记录的单调序号
struct MonoTime(Duration); // Runtime 注入的单调经过时间
```

一个 `Seq` 同时标识记录、询问关联、成果版本和路由请求。引用可以使用 `Seq`，不再引入 ArtifactId、MessageId、ReportVersion、MemoryGeneration。记录删除不回收序号，不能用数组长度生成序号。

Job 的 `revision: u64` 只在契约改变时递增。暂停、Stop 和其他使当前调用失效的操作直接撤销该调用的提交资格。CallId 在同一个 Runtime 内不复用，Stop 会撤销所有活跃调用及路由，因此第一版不再额外维护全局 generation。

第一版不自动重试业务操作，CallId 同时作为工具适配器可用的本次操作标识；不保留与 CallId 永久一一对应的 EffectId。`CallContext::id()` 只在本 Runtime 内唯一，外部幂等键必须组合宿主提供的 Runtime 或 Session 标识。重复完成事件按 CallId 去重。Unknown 写入只能由宿主确证，不能自动换一个 CallId 重发。

时间由 `Kernel::step(now, event)` 注入。Job 定时与询问期限保存绝对 MonoTime，Runtime 等待 `next_deadline()`；不为每次等待创建独立 timer actor 或 WakeId。

## 4. Job：终态只存一次，Running 与 Paused 从事实派生

```rust
pub struct JobSpec {
    pub goal: String,
    pub scope: String,
    pub done_when: String,
}

pub struct Assignment {
    pub spec: JobSpec,
    pub inputs: Vec<InputId>,
    pub evidence: Vec<Seq>,
    pub seed: Option<JobId>,
}

enum Owner {
    User,
    Job(JobId),
    Routing(Seq),
}

struct Job {
    spec: JobSpec,
    inputs: Vec<InputId>,
    owner: Owner,
    revision: u64,
    local_paused: bool,
    state: JobState,
    active_call: Option<CallId>,
    context: JobContext,
    report: Option<Seq>,
}

enum JobState {
    Ready,
    Waiting(WaitState),
    Finished(Arc<JobOutcome>),
}

enum WaitState {
    Tool(CallId),
    Until(MonoTime),
    User { question: Seq },
    Job { job: JobId, revision: u64 },
    Result { job: JobId, revision: u64, after: Seq },
    Inquiry(Seq),
    Coordination(Seq),
    Commit(PendingStep),
}

struct PendingStep {
    call: CallId,
    step: WorkStep,
}
```

JobId 是 jobs 表的 key，不再在内部 Job 中复制。`Owner::Job` 同时表示父子拥有关系与交付去向；User 根工作交付用户，Routing 根工作交付固定解释请求。调查的只读限制沿 owner 链派生，不再复制 investigation 标记到每个子工作。

`Assignment` 是模型可提交的创建参数。模型不能填写 owner、revision、state 或权限。直接 Delegate 的 owner 必定是调用者；协调创建的 owner 根据当前合法路由来源决定。seed 只表示记忆来源，不授予控制或写入能力。

公开状态由 `status(job)` 唯一派生，顺序固定：Finished 对应终态 → 自己或祖先暂停则 Paused → active_call 存在则 Running → Waiting → Ready。Job 临时回答询问时可以保留原等待原因；无需 previous_state、inherited_paused 或另一个 Worker 生命周期。

finished 时 active_call 必须为空。暂停撤销受影响的 active_call，并保留 Ready 或真实资源等待；`Waiting(Commit(_))` 丢弃候选后改为 Ready，暂停门禁仍然有效。恢复只清除指定 Job 的 local_paused。实际容量从 calls 表计算，已撤销但尚未结束的调用仍占容量。

正常结束时 `JobState::Finished` 与权威记录共享同一个 `Arc<JobOutcome>`，没有独立 `job.outcome: Option<_>`。报告只保存当前记录的 Seq，旧报告留在记录里。

## 5. 模型只能提交一项下一步

```rust
pub struct WorkProposal {
    pub note: Option<String>,
    pub report: Option<ReportDraft>,
    pub answers: Vec<InquiryAnswer>,
    pub step: WorkStep,
}

pub struct ReportDraft {
    pub summary: String,
    pub evidence: Vec<Seq>,
}

pub struct Completion {
    pub summary: String,
    pub evidence: Vec<Seq>,
    pub remaining: Vec<String>,
}

pub enum WorkStep {
    Continue,
    Tool(ToolCall),
    Delegate(Vec<Assignment>),
    Wait(Await),
    AskUser(String),
    Inquire { job: JobId, question: String },
    Coordinate(String),
    Read(ReadQuery),
    PublishResult(ReportDraft),
    Reply(String),
    Finish(Completion),
    Fail(Completion),
}

pub enum Await {
    Tool(CallId),
    After(Duration),
    Job(JobId),
    Result { job: JobId, after: Seq },
}

pub enum ReadQuery {
    Jobs { parent: Option<JobId>, after: Option<JobId> },
    Job(JobId),
    Record { id: Seq, offset: usize },
}
```

`note` 是局部工作笔记；Report 面向协调；answers 只回答本调用实际收到的关联询问。所有系统时间、提交版本和读取水位由 Kernel 绑定，模型不填写。

`WorkStep` 替代独立的 `operation + next + reply`。Tool 和 Finish 不能同时出现；Delegate 和 Finish 也不能同时出现。需要多个动作就用下一次调用推进，不引入通用 continuation、脚本执行计划或任意动作列表。唯一批量动作 Delegate 用于一次明确派出多个独立子工作。

**第一版 Tool 发出后等待该工具结果。** 不保留旧协议中工具与 Continue 任意组合的能力；同一 Job 至多一个未结束工具调用。询问可以在工具等待期间触发 Worker 判断，但第二个 Tool 必须等已有调用结束。独立并行工作用 Job 表达，全局仍有多个工具槽。此处以更少状态换取更清楚的行为。

工具阻塞从 calls 表检查，不能靠替换 JobState 绕过。只有新的必要投递，例如询问、查询回执、子 Outcome 或新要求，才能在工具等待期间触发一次临时判断；已读但未答的问题不会自行反复唤醒。临时调用保留原等待；Continue 在提交时保留仍成立的等待，否则 Ready。Read 投递查询回执后允许再次判断，不需要保存/恢复另一套 Worker 状态。

Delegate 校验整个列表、一次检查容量、原子创建全部子工作，并投递 ChildCreated；容量不足时一个都不建，把拒绝原因作为 Audit 附着到父 Context，并将父 Job 重新排队。父 Worker 可以选择本地执行、稍后再委派或其他步骤；容量不足本身不产生 Failed Outcome。Delegate 成功后父 Job Ready，不自动等待所有子工作。

Read 是只读 Kernel 记录查询，结果即时投递并重新唤醒调用者；不调用业务工具或额外模型。Jobs 按 JobId 翻页，parent 为空列活跃根工作，否则列直属子工作；Job 读一个已知工作卡片。目录页最多 16 条；Coordinator 查询页还必须在完整 CoordinateInput 的 `context_bytes` 预算内，不能只计算卡片本身。存在后页时 `next_job` 是本页最后一个 ID，调用者把它传给下一次 `ReadQuery::Jobs.after`。Record 按不可变正文的 UTF-8 字节偏移分段，Kernel 限制长度并维护字符边界，返回 `next_offset`。读取回执保存来源及范围，不再复制大正文。

Worker 可读自己记录、直属子工作的公开卡片/成果及显式共享记录；拥有子工作不自动开放它的私有工具历史。协调者可读同一宿主会话的公开目录、报告与允许的证据。Report、Published、Outcome 和 Inquiry Answer 的显式 evidence 向获准接收相应成果的调用者开放定向读取；导入记忆同样只授权 record_refs。未列出的私有记录仍隔离，不递归继承来源 Job 的全部权限。证据正文不因此自动展开，仍通过 Read 按需获取。该边界不声称多租户隔离。

PublishResult 是显式成果发布，不等于 Report 更新。发布记录 Seq 成为成果身份；WaitForResult 不由 note、错误日志或普通进度满足。Reply 是非终态用户答复，仅 User 根工作可以使用；最终正常答复从 Finish 的 Completion 产生。

`JobOutcome { kind, completion, revision, as_of }` 的 kind 只有 Completed / Failed / Cancelled。Finish/Fail 由 Worker 提出；宿主取消、仍有效的模型/压缩调用超时或异常由 Kernel 生成相应终态，不等待模型提供文本。普通工具失败先作为执行事实交给 Worker；Pause 引发的失效调用取消不终结 Job。Completed 要有非空 summary，summary 说明 done_when 如何满足；不能仅凭字符串和 evidence 非空证明领域目标正确。

### 协调协议同样使用互斥枚举

```rust
pub enum KernelDecision {
    Apply {
        changes: Vec<JobChange>,
        constraints: Option<String>,
    },
    Read(ReadQuery),
    Inquire { job: JobId, question: String },
    Investigate(Assignment),
    Clarify(String),
}
```

JobChange 只有 Create(Assignment)、Update { job, spec: Option<JobSpec>, action, inputs, required }。action 是 Keep / Pause / Resume / Cancel。Apply 的变更整组校验后提交，不能同时声明尚未解释输入又修改控制状态。

inputs 必须引用当前授权可用的原话，不能伪造 InputId。用户路由新建的 User 根工作成为相应输入的 required root；Update 的 required 只表示这些新关联输入是否等待该根工作交付，不能删掉已有义务。纯控制输入可以在控制应用后结算。Delegate 和调查创建不增加用户 required 集合，子工作向 owner 交付。

用户路由有原始 Input 依据；Worker 发起的协调保持既有授权边界：可以创建自己的子工作、处理自身或拥有子树的允许控制，不能改写原目标、解除用户暂停、制造新输入义务或控制无关工作。跨树调整若没有当前用户依据，询问用户而不是从报告推导新授权。

一次协调请求可以 Read、Inquire 或 Investigate 后继续，再以 Apply 或 Clarify 结束。路由是 Kernel 表中的普通记录，等待期间不占协调模型槽。

## 6. 一份正文，一组引用

```rust
struct Record {
    seq: Seq,
    origin: Origin,
    body: RecordBody,
}

struct JobContext {
    records: VecDeque<Seq>,
    read_through: Seq,
    checkpoint: Option<Arc<Checkpoint>>,
}

struct Checkpoint {
    job: JobId,
    revision: u64,
    through: Seq,
    summary: String,
    evidence: Vec<Seq>,
}

struct Inquiry {
    requester: Requester,
    target: JobId,
    target_revision: u64,
    deadline: MonoTime,
}
```

Kernel 用 `BTreeMap<Seq, Arc<Record>>` 保存正文。RecordBody 包含原始输入、委派说明、工作笔记、工具事实、报告、发布成果、Outcome、询问、回答、投递信封及导入记忆。Pause/Resume 的实际变更写成 JobControlChanged，供增量观察者更新状态。Origin 由 Kernel 填写用户 Input 或 Job/Call/版本来源，模型不能提供它。

inbox 是本地索引的投影，不再单独保存正文。`Delivery { to, source: Seq, kind }` 自己有一个新的记录序号，source 指向原成果。**今天收到昨天的成果，要用今天的投递序号判断未读，不能拿昨天的成果序号和读取水位比较。**

待答状态只存在 `Kernel.inquiries: BTreeMap<Seq, Inquiry>`。JobContext 不再存第二份 pending 集合；有界 Job 数下直接按 target 过滤。已读但未回答的问题每次仍进入上下文。结算时从表里移除并追加不可变的 Answer/Unavailable/TimedOut 等记录。

同一 Record 正文只有一份。Outcome 在 Job 终态和记录中共享 Arc；`DeliveryKind::Outcome` 信封只引用它。Call 当前状态保存最新执行结论，历史执行与确证记录保持不可变；不要把同一大输出再复制进 job.results、material 和 inbox。

这不是 event-sourcing 框架：不要求用记录重放整个 Runtime，也不增加 reducer event schema 的第二套映射。记录用于上下文来源、历史观察和明确投递。

## 7. ModelInput 从一开始就有范围

```rust
pub enum ModelInput {
    Coordinate(CoordinateInput),
    Work(WorkInput),
    Compact(CompactInput),
}

struct WorkInput {
    job: JobId,
    revision: u64,
    spec: JobSpec,
    constraints: String,
    waiting: Option<WaitView>,
    checkpoint: Option<Arc<Checkpoint>>,
    children: Vec<JobCard>,
    inquiries: Vec<InquiryView>,
    calls: Vec<CallView>,
    records: Vec<RecordView>,
    tools: Vec<ToolSpec>,
}

struct CompactInput {
    job: JobId,
    revision: u64,
    previous: Option<Arc<Checkpoint>>,
    through: Seq,
    records: Vec<RecordView>,
}

struct RecordView {
    source: Seq,
    origin: Origin,
    offset: usize,
    next_offset: Option<usize>,
    content: String,
}
```

RecordView 是一次冻结的读取范围；它携带来源序号、Origin、该段正文和后续偏移，不直接序列化整个 Record。Kernel 中的权威正文仍只保存一次，冻结输入只复制本次有界片段。

CoordinateInput 只含本次路由原话、约束、相关材料和一个当前查询视图。没有本路由 Read 结果时默认展示活跃根工作第一页；显式 Read 后，最新查询页替换默认首页。JobCard 从 Spec、真实状态及当前有效 Report 派生，不保存另一份可变目录。默认页与显式目录页都受 16 条及完整 CoordinateInput 的 `context_bytes` 约束；`next_job` 给出继续读取的游标。历史查询页仍留在权威 Record 中，但模型投影只显示本 routing 最新查询页，不随翻页持续累积旧页。合并输入时继承的旧路由查询页也不会替代新路由的默认首页。

所有模型输入在发出 Start Effect 前冻结；Runtime 不再按引用查询“最新 Report”。Arc 只共享不可变记录，不能出现 `Arc<Mutex<Job>>`。WorkInput 不携带全局 Snapshot、全局 calls 或其他 Job 的私有记录。

`context.rs` 只需三个普通读取函数：

```rust
fn prepare_work(kernel: &Kernel, job: JobId) -> Result<PreparedWork, ContextError>;
fn prepare_coordinate(kernel: &Kernel, routing: Seq) -> Result<CoordinateInput, ContextError>;
fn prepare_compact(kernel: &Kernel, job: JobId) -> Result<CompactInput, ContextError>;

enum PreparedWork {
    Work { input: Box<WorkInput>, seen_through: Seq },
    Compact(CompactInput),
}
```

具体内部模块可以读 Kernel 的 crate-private 数据；不为这一层创建仓储 trait 或依赖注入接口。准备函数不改状态。Call 保存 seen_through，模型不能填写或提高这个水位。

### 投影算法

1. 放入当前 Spec、当前约束、活跃直属子工作，以及仍运行或外部效果 Unknown 的本 Job Call。
2. 放入未读的必要投递、所有未结算询问及其关联信息。
3. 放入最新 checkpoint 和它之后的有效近期记录。
4. 自动附着的 Record、Delivery 及其 source 完整展开；ReadResult 回执也完整展开，只有它指向的显式 Record range 按 `item_bytes` 分页。按 `(Seq, offset)` 去重，完整正文不再与它的分页预览重复展开。
5. 工具输出在进入权威 ToolFinished 前按 `tool_output_bytes` 截成带原长度的预览；Record 分段按需续读。
6. WorkInput 超预算时压缩 checkpoint 之后、`read_through` 以内的连续前缀。没有可压缩前缀或必要 payload 仍放不下时返回 ContextError，不静默删当前要求。

seen_through 是本次实际提供的本地记录末端水位。有效 WorkProposal 提交后才推进 `read_through`；读取不等于回答 Inquiry。当前用户要求和未决问题不能用空引用代替。第一版要求本次投影整体装得下，否则压缩已读前缀或明确失败，不实现部分消费确认协议。已完成 child 和已知终态 Call 不进入默认卡片；其事实通过 Record 与 Read 保留。

### 预算与压缩

`AgentLimits::context_bytes` 计算序列化后的 `WorkInput`、`CoordinateInput` 或 `CompactInput` UTF-8 字节数；它是 Agent context payload 上限。`model.rs` 随后才加入 instructions、tool schema 和 provider options，也没有在这个数中预留输出，因此宿主必须依据真实模型窗口留出余量。超出 provider 限制时返回调用错误，不偷偷截断协议。

压缩用 worker 模型执行 Compact，仍走同一个 Call/Effect/Event 路径、占同一个 Worker 槽。同 Job 不同时运行 Work 和 Compact。模型仅返回 `CheckpointDraft { summary, evidence }`，覆盖水位来自 Kernel 在启动前选择的固定已读前缀。

压缩输入本身也必须有界：旧 checkpoint 加下一段可容纳的已读记录，不把已经超窗口的全部历史再次发给模型。成功必须推进 through；需要再压缩时让出调度槽，下一次处理后续前缀。不能反复压缩同一范围或把未读要求纳入已处理摘要。

提交 checkpoint 前检查 CallId、Job revision、暂停状态和基准 checkpoint。压缩不推进 read_through，不回答询问。新消息保留在后缀。失败按调用失败形成 Failed Outcome，旧记录和 checkpoint 保留供显式重试。

### 完成后的记忆复用

seed 只接受合法创建路径允许复用的 Completed Job：Worker 可选择自己的已完成子工作；用户路由可选择当前请求允许的会话记忆。仅能看见 JobCard 不授予完整 Context 读取权。Kernel 始终使用来源最终 Outcome 的 summary，并把 Outcome 自身序号和最终 evidence 纳入 record_refs；来源 checkpoint 的 evidence 可以补充背景，较早的 checkpoint summary 不覆盖最终结论。新 Job 收到 `ImportedMemory { source_job, source_revision, summary, record_refs }`，只对这些显式引用获得定向读取权。新 Job 的 checkpoint 为空，`read_through` 从零开始；旧 checkpoint 的 through 不会泄漏进新 Job。

首次输入只必须容纳来源身份和有界记忆摘要；record_refs 是可按需读取的背景证据，不把全部旧历史变成新 Job 必须展开的未读消息。无论来源有无 checkpoint，最终 Outcome 都是导入摘要的权威来源。保留来源的工作事实，不复制旧的未决问题、active_call、暂停标记、路由权或用户交付义务。当前进程不清理源记录；未来引入 retention 时必须为缺失引用定义明确的不可用语义。

第一版继续结构化模型协议，不依赖 provider 私有 replay。若将来启用原生历史，只保存真实 assistant 输出与真实处理回执；`submit_work.step` 里的业务工具不能伪造成模型直接发出的原生 tool_call。

## 8. Kernel 持有表，Runtime 持有 future

```rust
struct Kernel {
    limits: AgentLimits,
    jobs: BTreeMap<JobId, Job>,
    inputs: BTreeMap<InputId, InputEntry>,
    calls: BTreeMap<CallId, CallEntry>,
    records: BTreeMap<Seq, Arc<Record>>,
    inquiries: BTreeMap<Seq, Inquiry>,
    routings: BTreeMap<Seq, Routing>,
    interactive_ready: VecDeque<JobId>,
    background_ready: VecDeque<JobId>,
    routing_ready: VecDeque<Seq>,
    constraints: String,
    user_question: Option<JobId>,
    // 单调 ID。
}

struct Actor {
    kernel: Kernel,
    model: Arc<dyn ModelPort>,
    tools: BTreeMap<String, (Arc<dyn ToolPort>, ToolEffect)>,
    tasks: JoinSet<(CallId, Event)>,
    running: BTreeMap<CallId, RunningCall>,
    // 宿主请求/控制、观察通道、单调时钟起点、shutdown 状态。
}
```

InputEntry 只保存 Input Record 的序号、可选终态、required root jobs 和 routing ID；原话从权威 Record 读取。Routing 保存 requester、Input IDs、本次请求、查询记录、当前有效 Call 和等待状态；requester 是 Inputs 或带源 Job revision 的 Job。旧路由结果不能重新激活已经关闭的解释。

CallEntry 保存一个具体 CallTask、运行状态和最新进度。Work task 绑定 job/revision、seen_through 和本次 Inquiry IDs；Tool task 保留共享 request 及真实 effect。冻结输入随 Start Effect 交给 Runtime，不在 Kernel CallEntry 中再存一份。Job.active_call 是模型提交资格，calls/running 是实际执行生命周期，两者用途不同。

Tool 的 ReadOnly/ExternalWrite 由注册适配器声明，模型不能自报。Runtime 拥有取消信号、任务句柄和真实超时；Kernel 拥有执行资格、动作许可及外部效果事实。

## 9. 提案处理：先校验整体，再提交事实，最后执行步骤

```rust
fn accept_work(&mut self, call: CallId, proposal: WorkProposal, effects: &mut Vec<Effect>) {
    // 1. 确认仍有提交资格。
    // 2. 用只读检查校验整份提案的结构、引用、关联回答和 owner 边界。
    // 3. 提交 note / report / answers / 实际读取水位。
    // 4. 独立处理 step：执行、暂扣，或要求重新判断。
}

fn try_step(&mut self, job: JobId, pending: PendingStep, effects: &mut Vec<Effect>);
fn finish_job(
    &mut self,
    job: JobId,
    kind: OutcomeKind,
    completion: Completion,
    effects: &mut Vec<Effect>,
);
```

这不是通用两阶段事务。Kernel 串行执行且不 await，先完整校验再修改即可。没有“复制整个 Kernel → 生成 mutation plan → 重放计划”的中间语言。

公共部分与受门禁限制的 step 必须分开：**answers 已提交后，即使 Finish 被未解释输入暂扣，协调者也必须能拿到内部回答。** 否则协调者等回答、Finish 又等解释完成，会互相等待。

候选只保存尚未执行的 WorkStep 和原 CallId，不保存整份 WorkProposal。重新放行候选不重复写 note、报告或回答。执行时再次检查 owner、revision、消息水位、暂停、依赖和容量；已结束的 Call 不再有模型提交资格，但可作为这项已接受步骤的不可变依据。

结构错误或越权引用使整份提案拒绝，进入明确调用失败路径，没有局部更新。本调用确实收到过、但现已超时/取消/结算的询问，其回答是过时 no-op，只留审计，其余有效内容继续提交；未知或不属于本调用的关联 ID 才是结构错误。容量不足属于可观察等待或拒绝接收，不等于结构错误。Delegate 容量不足将拒绝 Audit 写入父 Context 并重新调度，让父 Worker 重新决定，不封存 Failed Outcome；Tool、对外 Reply、Finish 的暂时门禁可保存 PendingStep。

`try_step(Tool)` 同时检查本 Job 没有未结束工具、实际工具槽可用和动作许可。ExternalWrite 还必须通过未解释输入门禁，并且会话没有其他未决写入，包括 Unknown；第一版继续串行外部写，不建立资源锁管理器。Routing 调查及其拥有子树只能使用 ReadOnly 工具。

如果新要求、新子 Outcome 或新询问使候选的依据过时，移除候选并排入 Ready；已经提交的内部回答不撤回，后续状态变化通过新记录说明。普通进度不使候选失效。

### 正常 Finish 的唯一检查入口

`try_step(Finish)` 依次检查：当前契约/暂停有效；本次 answers 提交后没有未决询问；必要投递已读；没有本 Job 未完成的工具；所有拥有子工作已终态且 Outcome 已读；**整个拥有子树没有 Running、CancelRequested 或 Unknown 的 ExternalWrite**；用户输入解释门禁允许对应交付。调查及其拥有子树的内部 Outcome 可穿过用户交付门禁，回到所属解释路由；其他完成条件仍需满足。

子工作 Cancelled 不表示它发出的外部写已经撤回，因此不能只看子状态。失败/取消可立即封存 Outcome 并撤销子树资格，不必等待这些成功前置条件。

finish_job 是唯一 Job 终态写入点。失败或取消先递归封存未完成子树，再撤销调用并结算相关 Inquiry；随后用即将分配的 Seq 构造 Outcome，同一 `Arc<JobOutcome>` 同时写入 Record 与 Finished 状态，并向仍活跃的 owner 和 waiter 投递一次。`finish_inputs` 在同一次 advance 中根据 required roots 单独产生 InputFinished。父工作已经终态时只保留 Outcome 观察，不修改它的冻结 Context。

## 10. 内部询问与输入解释共用明确归属

`inquire(requester, target, question)` 在创建询问记录时绑定 target_revision 和 deadline，并向目标添加新的投递信封。Requester 只能是内核确认的 Job 或 Routing，不接受模型伪造来源。

每 requester/target 至多一个未决询问。Finished 始终优先返回 Outcome 引用；非终态目标 Paused 或不可访问时立即返回明确结果。目标活跃时，在下一次可运行边界回答；不为答疑产生第二个有效 Worker。

读取水位不结清询问。只有关联 Answer、NeedsWork、Unavailable、Cancelled 或 TimedOut 才移除 inquiries 中的项。暂停目标时结算它已经接受的询问为 UnavailablePaused，避免请求者无期限等待。目标 revision 改变时以 Changed 结束旧询问，旧答案不能冒充新目标的回答。

询问等待与 Job/Result 等待一起做有界依赖遍历，在提交时拒绝环；不引入通用图引擎。取消请求者会关闭其询问，迟到回复不重新激活请求。

Finish 候选出现后拒绝普通新询问，排空有限已接受问题。当前有效输入解释路由保留一个关联询问入口；接收它会废弃旧 Finish 候选。内部回答可以穿过普通交付门禁，但只能回原 requester，不能向用户发布答案或启动业务写入。

解释输入时对已完成工作的跟进，其 owner 是当前 Routing；由此沿拥有关系获得调查写限制，Outcome 仅内部返回，不因所属路由仍在等待自己而被用户交付门禁扣住。Worker 不能把普通 User Job 自行改成 Routing Job。

新的普通 Input 通过接收检查后，与所有未关闭输入路由的原话按 accepted_at 顺序合并成新批次，包括 Failed、WaitingForUser、WaitingInquiry、WaitingJob 和仍有在途 Coordinate 的路由。旧路由关闭并清除 active_call，旧 inquiry 与调查树被取消；旧 Call 的迟到结果只记事实，不能恢复解释权。原 InputEntry 改关联到新路由，finished 与 required_jobs 保留，不因批次取代生成 InputFinished；已关闭路由中等待业务 Job 交付的输入不重新解释。Retry 只查看 Input 当前关联的路由，不能重开被取代的旧批次。已发生的工具效果仍记账。

用户问题由 `Kernel.user_question: Option<JobId>` 全局串行。Job 状态中的 `WaitState::User { question }` 指向权威 Clarification Record；回答必须用新的 InputId 和 `reply_to` 明确关联。当前 Job 可以在没有开放输入路由时替换自己的问题；输入解释进行中则继续保留旧问题，不进入不可回复的 Commit。

## 11. 主循环与调度顺序

```rust
loop {
    let event = tokio::select! {
        control = controls.recv() => control_event(control),
        input = inputs.recv() => input_event(input),
        result = tasks.join_next(), if !tasks.is_empty() => completion_event(result),
        _ = sleep_until(next_deadline) => Event::Tick,
    };
    let effects = kernel.step(elapsed(), event);
    for effect in effects {
        dispatch(effect); // 启动、请求取消、通知；不等待模型或工具结束。
    }
}
```

代码为简图；实际处理通道关闭和没有 deadline 的分支。Kernel 只输出 Start、Cancel、Notify 三类 Effect；内部 Read 直接处理记录，不需要启动 future。调用超时由 Runtime 的 future 监督，Tick 只处理 Job 定时与询问期限。

`step(now, event)` 顺序固定：

1. 结算 `deadline <= now` 的询问和定时等待；同一时刻到达的超时优先于回答。
2. 处理本次事实；CallFinished 先记实际结束，再判断模型输出是否仍可提交。
3. 应用合法模型提案、控制和投递，唤醒受影响的等待者。
4. 处理本轮开始时已有的候选与可结算 Input，各队列至多扫描一次。
5. 按容量安排协调、Worker 或 Compact 调用，冻结输入并产生 Start。

不使用 `while state_changed` 无限自触发循环。由本轮内部操作产生的就绪项，在同一步的调度阶段处理；每次只启动受容量限制的调用，下一次进展需要真实新事件。

保留 1 个协调槽、1 个交互 Worker 保留槽、可配置后台 Worker 槽与工具槽。交互和后台各用一条去重 FIFO；调度先取交互工作，后台同时受自己的槽数限制。等待不占模型槽，CancelRequested 调用在真实结束前仍占槽。控制通道与普通输入分开，由 Tokio 选择下一个事实。

Worker 的就绪条件只写在 `runnable`：未终态、未暂停、无有效模型调用、不是 Commit，并且状态为 Ready 或存在未读必要投递。定时、工具、依赖、Inquiry 和 Coordination 的结算路径负责把相应等待改成 Ready；必要投递也能在保留真实等待原因时临时唤醒 Worker。Running/CancelRequested 仍是本地未结束工具；已经结束但效果 Unknown 的调用只走写入/Finish 门禁，不能阻塞必要推理与只读核对。

Stop 把当时全部非终态 Job 封存为 Cancelled，关闭旧 Routing 和询问，清除相应候选与就绪项，并发出取消指令；不是仅清 active_call 后把旧 Ready 工作留下。随后接受的新输入可以创建新工作，旧 CallId 永远不能成为新 active_call，晚到事实不重新激活终态。shutdown 再停止接收新输入，等待本地清理期限，并返回仍未确定的调用。

最后一个 Agent handle 被丢弃后，宿主通道关闭，Actor 自动执行 Stop 并进入 shutdown 清理。它不因内部 progress sender 仍存在而继续保留无人持有的工作。丢弃单个 clone 不影响其他 handle；需要取得 ShutdownReport 时使用显式 shutdown。

## 12. 暂停、改目标、迟到结果

有效性只依赖 active_call、Job revision、暂停拥有链和 Routing 的 active_call，不增加 generation。相关检查集中在少数已有入口：

```rust
fn is_paused(&self, job: JobId) -> bool; // local_paused || 任一 owner Job 暂停
fn invalidate_job_call(&mut self, job: JobId, effects: &mut Vec<Effect>);
fn invalidate_job_calls(&mut self, job: JobId, effects: &mut Vec<Effect>);
fn invalidate_job_routings(&mut self, job: JobId, effects: &mut Vec<Effect>);
fn owns(&self, root: JobId, target: JobId) -> bool;
```

Pause 清除子树有效调用及待提交动作，保留真实等待原因，并请求取消正在执行的调用；Commit 等待随候选一起清除并变为 Ready。Resume 只清除指定 Job 的 local_paused；祖先仍暂停时，该 Job 继续受继承门禁约束。实际 Pause/Resume 变更写入 JobControlChanged，增量观察者无需重新读取完整快照才能知道控制状态。原本 Ready 的 Job 恢复后可调度；工具已结束或定时已到的等待也可重新检查。Tick 消费已到期限，即使暂停也不继续把过期时刻作为 next_deadline，避免空转；暂停门禁保持。任何内核材料都不能隐式 Resume。

Spec 改变则增加目标 Job revision、记录 JobChanged、取消其活跃拥有子树，并用 DependencyChanged 唤醒绑定旧 revision 的等待者。已完成子工作及其成果可继续作为历史证据。同一个协调决定保守地拒绝同时更新 owner 与 descendant，避免前一项终结后一项。路由确认新用户要求必须由某 Job 处理时，即使 Spec 文字不变，也撤销该 Job 的旧模型调用与候选；全局 constraints 真正改变时撤销所有活跃 Job 的旧模型资格、Coordination 路由和在途工具。工具晚到的真实效果仍入账。普通补充资料和 Inquiry 不引发整棵树重算。

迟到模型结果只留审计，不提交 note、Report、answers 或 checkpoint。迟到工具结果总记真实效果及原 revision；Unknown 被确证时更新 Call 当前事实、追加新记录，并使相关仍活跃的工作重新考虑依赖旧 Unknown 的候选。

等待绑定目标 Job 的 revision。目标契约变化返回 DependencyChanged，重新唤醒消费者，不能把新目标的成果当成旧等待的结果。已消费的固定成果内容不被后续报告覆盖。

## 13. 当前容量边界与后续 retention

AgentLimits 是带公开字段的普通配置值，以 `Default` 配合 struct update 修改，并由一个 `validate` 检查。`Agent::with_ports` 创建 Kernel 时执行这一次验证，不由模型适配器拼装或暗改默认值。

当前实现的硬边界只有：模型/工具/关机超时、后台 Worker 与工具并发、活跃 Job 数和深度、普通 pending Input、未决 Inquiry、单次 context payload、单项 summary/note/checkpoint 及工具输出。普通输入满时仍保留一个正在等待的 clarification reply 信封；Delegate 在活跃 Job 容量不足时整批拒绝。相同 CallProgress 值不重复记录，不同进度仍是独立事实。

`context_bytes` 只限制序列化的 Agent context DTO。Worker 默认不投影已完成 child 和已知终态 Call；协调者默认只投影活跃 root 的有界页；checkpoint 替代模型输入中已经读过的 Record 前缀。这些规则控制每次模型看到的内容，不回收 Kernel 的权威历史。

当前 `records`、`jobs`、`inputs`、`calls`、`routings` 与每个 Job 的记录序号随进程生命周期保留；没有记录总预算、容量预留或 GC。这样保住了 InputId 幂等、Delivery 读取授权、Outcome/evidence 引用和 Unknown 写入真相，但意味着本版本不承诺进程总内存有界。

物理 retention 应作为一个完整后续改动：先定义保留根和引用闭包，再定义幂等 receipt 的寿命、observer cursor/lag 恢复、正文被回收后的 Unavailable 语义和宿主归档边界。不能仅按 checkpoint.through 删除 Record，因为 Delivery 与 ReadResult 仍用这些序号授予访问权。

## 14. 已实现范围与验证

新内核已经直接按目标类型实现，没有旧/新双运行路径。测试围绕行为边界，不绑定旧字段布局。

| 顺序 | 实现内容 | 交付检查 |
| --- | --- | --- |
| 1 | JobSpec、互斥 WorkStep、JobState/Outcome、Owner | 非法步骤组合不可表达；终态只写一次；模型不能填写拥有者 |
| 2 | Records、JobContext、冻结 WorkInput/CoordinateInput | A 的输入不含 B 私有正文；投递序号正确；同正文不重复展开 |
| 3 | Kernel 创建、提案提交、Delegate、成果等待、Finish | 容量拒绝不终结父工作；子树 Running/CancelRequested/Unknown 外部写阻止成功交付 |
| 4 | Runtime 端口、取消、时间、轮转与观察 | 取消资格后仍计实际容量；控制不等模型；慢观察者不阻塞执行 |
| 5 | Inquiry、Routing、seed 与 Compact | 内部回答不被 Finish 门禁扣住；旧批次不复活；最终 Outcome 是 seed 摘要权威来源，旧 checkpoint 不污染新 Job 水位 |
| 6 | 提示词/schema、Agent、示例及 API 文档 | ModelAdapter 的精确协议与无网络 walkthrough 跑通完整请求；明确说明 API 变化 |

优先保留三种测试层次，不增加测试框架：

- Kernel 表驱动事件序列：先后颠倒 Finish/询问/取消/改目标/子 Outcome，并验证确切 Effect 与状态。
- Context 固定样例：内容范围、未读与未答的区别、去重、预算、固定前缀压缩及跨 Job 记忆导入。
- Runtime 受控 futures：调用异常、取消后的真实外部效果、shutdown，以及最后一个 handle 释放后的端口清理。定时交错、负载调度和观察者掉队仍应在后续增加专门测试。

回归范围包括输入幂等与批次取代、Delegate 原子接收与容量拒绝、Context 隔离和显式证据共享、父子完成门禁、最终 Outcome seed、Pause/Resume 控制记录、Stop、迟到调用事实、澄清回复、调查内部交付、依赖 revision、目录预算及固定前缀压缩。Runtime 覆盖外部写取消、最后一个 handle 释放和调用异常清理；模型协议覆盖精确结构。具体命令结果以对应提交的验证记录为准，不在此维护容易过期的测试数量。

纯 Rust 行为测试不证明模型能正确拆分、总结或选择证据。真实模型评估仍需比较原话要求保留、错误完成、无谓委派、重复调查、context payload 和完成耗时；物理 retention 也仍是单独的工程阶段。

本设计的完成标准是：每条消息有归属、每份事实有唯一正文、每次调用读冻结视图、每项交付只有一个终态写入点。最小实现靠这些局部不变量成立，不依赖模型记住隐藏约定。
