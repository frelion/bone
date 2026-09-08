# bone-app 架构设计初稿

日期：2026-09-08。状态：供评审的重写方案，尚未实施。

本设计允许完整重写 `bone-app`，并把 TUI 移为独立前端。现有配置、凭据和 SQLite 组件按职责复用；旧 App 类型和模块结构不作为兼容约束。本文是新的设计入口，[旧 Agent API 迁移笔记](bone-app-agent-migration-plan.md) 仅保留为接口行为参考。

## 1. 定位与关键决定

`bone-app` 是 BONE 的应用后端。它把模型、工具、Agent 和存储装配成用户可操作的会话，并提供与界面无关的 Rust API。

第一版确定以下选择：

| 问题 | 决定 |
| --- | --- |
| 对外对象 | `App` 和 `Session` 两个句柄；其余主要是数据结构 |
| 前端边界 | TUI 独立 crate，只调用 App API |
| 执行单位 | 一个 Session 同时最多一个 Agent Runtime，多个 Job 在该 Runtime 内执行 |
| 配置生效 | 创建 Runtime 时解析并固定；保存的新配置作用于下一 Runtime |
| 输入回执 | 返回成功表示输入已持久保存；Agent 接受、等待、完成分别表达 |
| 输出 | 当前状态用 `watch`；历史用同一份 SQLite journal 分页读取 |
| 持久化 | 将 `bone-store` 合入 App 私有 `storage` 模块，复用 SQLite 实现与测试 |
| 重启 | 恢复会话与历史；旧执行标为中断，新执行可参考历史重新工作 |
| 写工具 | 具体的 Patch/Bash 适配器，加执行前后落盘；同一 Workspace 的写调用在 App 内串行 |
| 扩展方式 | 具体模块和普通函数；沿用已有 `ModelPort`、`ToolPort` 扩展点 |

App 不参与拆分 Job、调度 Worker、压缩 Job 上下文或判断 Job 能否提交。这些继续由 `bone-agent` 负责。

## 2. Crate 与依赖

```mermaid
flowchart TB
    TUI[bone-tui：终端交互与 bone 二进制] --> APP[bone-app：应用 API]
    OTHER[后续桌面 / Web / 自动化调用方] --> APP
    APP --> AGENT[bone-agent：执行与状态内核]
    APP --> LLM[bone-llm：模型连接与协议]
    APP --> TOOLS[bone-tools：文件与命令执行]
    APP --> STORE[内部 storage 模块：SQLite]
    AGENT --> LLM
    AGENT --> TOOLS
```

第一版只实现进程内 Rust API。以后需要 HTTP、SSE 或桌面 IPC 时，在 API 外加传输适配，不把网络路由、连接或终端事件传进 App。

`bone-tui` 依赖 `bone-app`，不直接依赖 `bone-agent` 或数据库。`ratatui`、`crossterm`、`ratatui-textarea` 和终端启动代码移到 `bone-tui`。二进制仍可叫 `bone`，one-shot 命令也调用相同 App API。

现有 `bone-store` 的唯一业务消费者就是 `bone-app`；Agent、LLM、Tools 都不依赖它。它的独立 crate 边界目前没有第二个实际使用者。重写时将 SQLite 连接、事务、journal、lease 和相关测试搬入 `bone_app::storage`，移除独立的 `bone-store` crate。保留已有实现，不重新发明数据库层；路径选择、业务记录和存储实现也由同一个 App 统一管理。

`storage` 是私有模块。其他 App 模块调用具体业务方法，如 save_input、append_records、read_history，不把 DocumentKey、JournalKey、SQL 或通用 transaction API 暴露给前端。内部现有泛型辅助代码有用就保留，无需为了合并再加一层转发包装。

TUI 负责键盘、布局、焦点、滚动、输入框编辑和渲染。App 的状态中不出现 `UiSessionId`、panel、scroll offset 或终端可用性。显示偏好保存在前端自己的配置中；Session 草稿作为可选产品数据通过 App 保存，文本编辑过程仍属于前端。

## 3. 对象与所有权

### 3.1 App

一个 App 实例持有一个内部 `Store`、配置与凭据组件，以及已打开 Session 的注册表。它能管理多个 Workspace 和 Session。

```text
App
├── storage::Store
├── 配置 / Profile / Credentials
├── Workspace 数据与进程内写锁
└── SessionId → SessionTask
                 ├── SessionWriter
                 ├── 输入与历史的应用状态
                 ├── watch<SessionView>
                 └── Option<RunningAgent>
                      ├── RuntimeId
                      ├── 固定的 RuntimeConfig
                      ├── Agent
                      └── Agent 观察水位
```

注册表的短锁只保护查找和插入，不跨数据库、网络或模型调用持有。数据库的短同步事务通过 `spawn_blocking` 执行。模型连接和退出收尾作为 SessionTask 持有的异步操作运行。

### 3.2 Session

`Session` 是稳定业务对象。它有自己的 Workspace、标题、输入、历史和配置覆盖。打开历史不启动模型；首次需要执行输入时再启动 Agent。

每个打开的 Session 对应一个具体的 `SessionTask`。它按顺序接受 App 命令、处理 Agent 观察结果、提交存储变更。这里的串行化只决定应用操作的顺序；Agent 内部仍可并发执行多个 Job。

同一 App 多次取得同一 Session，返回同一个任务的克隆句柄。App 注册表持有任务，前端释放 `Session` 或停止观察不会取消工作。跨进程继续使用现有 Session writer lease；另一个进程不能取得同一 Session 的执行所有权。

当前方案支持多个客户端连接同一个 App。它不承诺多个独立 App 进程同时执行同一 Session，也不需要为此引入分布式协调。

### 3.3 生命周期

这些操作分别定义：

| 操作 | 行为 |
| --- | --- |
| 前端断开 | 释放该前端的订阅，Session 和 Agent 继续存在 |
| `stop` | 取消当前及此前已排队的工作；保留 Runtime，可以再提交 |
| `close_runtime` | 拒绝这段关闭期间的新提交，收尾并释放 Agent；保留 Session |
| `archive` | 只改变 Session 的组织状态；要求先关闭 Runtime，不隐式取消工作 |
| `App::shutdown` | 收尾所有 Session 的 Runtime，保存结果，释放资源 |

SessionTask 不因为暂时无 Job 运行就自动销毁 Agent。同一 Runtime 内的后续输入可以继续使用其任务与记录上下文。

## 4. 公共 API

以下是目标接口形状，类型细节可在实现中收敛；不是当前已存在的 API。

### 4.1 App 入口

```rust,ignore
impl App {
    async fn open(options: AppOptions) -> Result<App>;

    async fn open_workspace(path: PathBuf) -> Result<WorkspaceInfo>;
    async fn create_session(workspace: WorkspaceId, title: String) -> Result<Session>;
    async fn session(id: SessionId) -> Result<Session>;
    async fn list_sessions(workspace: WorkspaceId) -> Result<Vec<SessionInfo>>;

    async fn config(scope: ConfigScope) -> Result<ConfigSnapshot>;
    async fn update_config(change: ConfigChange) -> Result<ConfigSnapshot>;
    async fn resolved_config(session: SessionId) -> Result<ResolvedConfig>;

    async fn profiles() -> Result<Vec<Profile>>;
    async fn save_profile(profile: Profile) -> Result<()>;
    async fn set_api_key(profile: ProfileId, key: SecretString) -> Result<()>;
    async fn login(profile: ProfileId) -> Result<LoginAttempt>;
    async fn logout(profile: ProfileId) -> Result<()>;

    async fn shutdown() -> Result<AppShutdownReport>;
}
```

`AppOptions` 只放宿主启动条件，例如数据目录；模型、工具配置有各自的持久化 API。测试和嵌入使用临时数据目录，App 自己构造 Store。Store 注入只作为 crate 内测试辅助，不成为公开 API，也无需存储 trait。

Workspace 是路径与身份的数据对象，不再增加一层必须串联使用的 service handle。前端需要的 profile/model 配置 DTO 从 App 导出，不需要自行连接模型。

### 4.2 Session API

```rust,ignore
impl Session {
    fn id(&self) -> SessionId;

    async fn submit(&self, input: SubmitInput) -> Result<SubmissionReceipt>;
    async fn retry(&self, input: InputId) -> Result<CommandReceipt>;
    async fn control(&self, target: JobRef, action: JobControl)
        -> Result<CommandReceipt>;
    async fn stop(&self) -> Result<CommandReceipt>;

    async fn snapshot(&self) -> Result<SessionView>;
    fn observe(&self) -> watch::Receiver<Arc<SessionView>>;
    async fn history(&self, after: SessionSeq, limit: usize) -> Result<HistoryPage>;
    async fn evidence(&self, reference: EvidenceRef, page: TextPage)
        -> Result<EvidencePage>;

    async fn rename(&self, title: String) -> Result<()>;
    async fn save_draft(&self, draft: String) -> Result<()>;
    async fn close_runtime(&self) -> Result<CloseReport>;
    async fn archive(&self, archived: bool) -> Result<()>;
    async fn resolve_write(&self, target: CallRef, resolution: WriteResolution)
        -> Result<CommandReceipt>;
}
```

`snapshot()` 请求一次新鲜的应用状态，适合明确查询；`observe()` 立即提供最近状态并持续通知变化。它们返回 App 自己的 DTO，不返回 `Agent`、`SessionWriter` 或原始 Kernel 控制入口。

`CommandReceipt` 只有 `Applied` 和 `Unchanged` 等有意义结果；错误使用 `NotFound`、`StaleRuntime`、`InvalidState`、`Busy`、`NeedsModel`、`LoginRequired`、`Storage` 等具体变体。App 核对 Runtime 身份，Agent 在应用控制的同一步返回实际结果。给现有 Agent 控制方法补这一小项回执，不用 App 先查询再猜测是否发生变化。

### 4.3 身份

| ID | 含义与范围 |
| --- | --- |
| `WorkspaceId` / `SessionId` | 产品的稳定身份 |
| `RequestId` | 调用方提供的 UUID；一次逻辑提交的重试键，在 Session 内去重 |
| `InputId` | App 为已保存输入分配的 Session 内递增编号；直接用于 Agent InputId |
| `RuntimeId` | 每次创建 Agent 分配的 UUID |
| `JobRef` / `CallRef` | `{ runtime: RuntimeId, id: local_id }` |
| `QuestionId` / `EvidenceRef` | 带 RuntimeId 的记录引用，由 App 返回，前端原样带回 |
| `SessionSeq` | Session journal 的位置，跨 Runtime 单调递增 |

不再建立与 Input 一一重复的 Turn 对象。`RequestId` 解决提交回执丢失后的重试，`InputId` 标识已经进入产品历史的输入。

## 5. 输入与控制契约

### 5.1 输入结构

```rust,ignore
struct SubmitInput {
    request_id: RequestId,
    text: String,
    reply_to: Option<QuestionId>,
}

struct SubmissionReceipt {
    input: InputId,
    saved_at: SessionSeq,
}
```

第一版支持文本输入。附件、多模态和工具调用注入不加入此接口。

App 公开问题引用，内部确定其关联的可回复 Input。当前 Agent 只接收 `reply_to: InputId`，无法区分同一输入先后出现的两个问题，因此把它收紧为包含 InputId 与问题 Seq 的回复引用，由 Kernel 在接受回答的同一步核对问题身份。App 负责 Runtime 身份，Kernel 负责当前问题；前端只带回 QuestionId，不理解 Routing。

一个 Input 可以关联多个必要 Job；多个 Input 可以更新同一个 Job。App 以 InputId 分别追踪状态，允许用户在执行中继续输入。

### 5.2 保存与投递

```mermaid
sequenceDiagram
    participant C as 调用方
    participant S as SessionTask
    participant D as App Storage
    participant A as Agent
    C->>S: submit(request_id, text)
    S->>D: 事务：输入 + 去重映射 + InputSubmitted
    D-->>S: commit
    S-->>C: SubmissionReceipt
    S->>A: post(Input)
    A-->>S: InputReceipt
    S->>D: 保存投递状态与后续事实
    S-->>C: watch 通知状态和历史水位变化
```

SessionTask 从收到命令起拥有整个提交操作。调用方取消等待、关页面或丢失回执，不取消已进入任务的操作。用同一 RequestId 重试：内容相同返回原回执，内容不同返回冲突。去重先于当前问题有效性检查，已经回答成功的请求再次提交仍取得原回执，不因问题已关闭而报错。

输入记录本身保存投递状态，不另建通用 outbox。基本状态为：

```text
Queued → Accepted → Finished(Completed / Failed / Cancelled)
   │          ├── WaitingForUser
   │          └── RoutingFailed
   ├── 依赖未就绪或启动失败：保留 Queued，带原因
   └── Agent 明确拒绝 → Rejected(reason)

丢失旧 Runtime 的非终态输入 → Interrupted
```

`WaitingForUser`、`RoutingFailed` 是可继续状态，完成后仍由对应 `InputFinished` 结算。`Interrupted` 是 App 对执行丢失的记录，不伪造 Agent 的 Failed 或 Cancelled。输入已保存后，Agent 仍可能因问题刚刚失效而返回 InvalidReply；此时写入 InputRejected 并结算为 Rejected，不能永久留在 Queued 或向已返回的 submit 再抛一次错误。Busy 属于暂时未接受，继续 Queued。

保存配置缺失、登录未就绪等原因后，调用方可以先修正配置或登录，再显式 `retry(input)`。`retry` 只处理尚未投递的输入或同一 Runtime 的 RoutingFailed；已经 Finished/Interrupted 的输入不自动重做，要通过新的 submit 表达新工作。

Agent 容量满时，已保存输入留在 Queued。SessionTask 按接收顺序投递，并在状态变化后再试，不为每条输入另建 timer 或 retry worker。排队量沿用配置中的输入容量限制；达到上限直接返回 Busy，不无限接收。

### 5.3 stop 与提交顺序

App 的 submit、retry、stop 和 Job control 进入同一个 Session 命令队列。

当 stop 被处理时：

1. 此前已经保存但尚未投递的输入结算为 Cancelled。
2. 已进入 Agent 的输入通过 Agent stop 结算。
3. 此后进入 Session 队列的新提交可以开始新工作。

同一 Session 的 `Agent::post` 和控制调用按此顺序发出并等待各自接受回执，避免 Agent 内部分开的 input/control 通道导致停止后又交付旧排队输入。模型连接可以异步进行，但启动完成必须回到 SessionTask 核对当前状态，不能自行投递。

## 6. 输出、观察与历史

### 6.1 SessionView 是当前状态

```rust,ignore
struct SessionView {
    session: SessionInfo,
    runtime: RuntimeState,          // Detached / Starting / Running / Closing
    inputs: Vec<InputView>,         // 当前未结算输入与待答问题
    jobs: Vec<JobView>,             // 当前运行时的工作概况
    activity: Vec<CallView>,        // 正在执行的调用与最新进度
    unresolved_writes: Vec<WriteView>,
    history_through: SessionSeq,
    problem: Option<AppProblem>,    // 当前启动或存储等应用问题
}
```

这些 View 属于 App：例如 Job 的等待状态转换为调用方能展示的原因、对象引用和摘要。它们不包含 Kernel 的 Ready 队列、Delivery 路由或可执行状态。

Agent 的 `AgentView` 是 Input↔Job 关系和 Job 状态的权威来源。当前 Record 没有包含全部状态变化，App 不靠 Record 自己再实现一次 Kernel。具体做法是在一批观察通知后刷新 AgentView，转换并发布应用 View。

View 中的历史水位只指向已成功保存的事实。进度可以只更新内存，不为每次 percent 变化落盘；调用结束、回复和输入终态则先保存再发布。

### 6.2 历史由 App 解释

公开历史条目使用产品类型，例如：

| Session 历史条目 | 含义 |
| --- | --- |
| `InputSubmitted` | 用户输入已保存 |
| `InputAccepted` | 运行时已接受输入 |
| `QuestionAsked` | 问题、QuestionId、关联输入 |
| `Reply` | 发言正文、JobRef、关联输入 |
| `JobFinished` | 某项工作结束，含摘要、剩余事项和证据引用 |
| `InputFinished` / `InputRejected` | 某条输入完成执行，或已保存但被明确拒绝执行 |
| `ToolFinished` | 工具调用结果与副作用结论 |
| `RuntimeStarted` / `RuntimeClosed` | 一段执行实例的生命周期 |
| `Interrupted` / `WriteResolved` | 中断和后续核查事实 |

子 Job 的结果和面向用户的发言保持独立。App 为 JobFinished 标明它是 root 还是 child，以及关联输入；前端可以折叠子结果，不必猜哪个 Outcome 代表整条需求结束。

`Reply` 不意味着工作完成；直接 Finish 的 Job 也有可展示的 JobFinished。one-shot 等待自己的 InputFinished，遇到自己的问题或 RoutingFailed 则返回对应状态，不等待任意子 Job 或任意模型调用结束。

工具事实用 CallRef 标识。Unknown 后得到已知结果，更新同一次调用的事实；不能显示成第二次工具执行。当前 Agent 工具结束事件是 ToolFinished，不再等待一个不存在的第二条 CallFinished。

### 6.3 一个状态通知加一个分页接口

```rust,ignore
struct HistoryPage {
    items: Vec<HistoryEntry>,
    next_cursor: SessionSeq,
    has_more: bool,
}
```

前端接入流程固定：先 observe 取得 receiver，读取其当前 View，再调用 history 补到 `history_through`；之后每次 watch 变化，更新状态并继续补读历史。

`watch` 可以合并通知。回复和结果保存在 SQLite，前端不会因为少接收一次通知而丢消息。客户端重连带上最后的 history cursor 即可；不持有 cursor 的客户端从零读取。

不增加 App broadcast、客户端消费确认、EventBus 或独立历史推送缓存。未来的 SSE 适配可以封装相同的 watch + history 流程。

### 6.4 App 内部接 Agent

SessionTask 保存当前 Runtime 的 `agent_through`。收到 Agent 记录或 Lagged 时，用 Agent observe 获取新的快照与订阅：

1. 取出快照中超过已保存水位的记录，按 Seq 处理。
2. 在事务中归档记录、更新应用输入状态与最近应用快照。
3. 事务成功后推进水位，发布由同一快照生成的 SessionView。
4. 用新 receiver 继续观察，丢弃旧 receiver。

一次调度周期可以合并多个通知，避免每条进度都刷新完整快照。这是第一版的直接实现；Agent 当前返回完整保留记录，超长 Runtime 的增量查询优化留在 Agent API，不在 App 增加另一套执行状态缓存。

存储失败时不推进已保存水位，不发布“已保存的完成”。Session 停止接收新执行，尝试停止并收尾现有 Agent，在 View 中暴露存储问题；恢复存储后仍可从活着的 Agent 快照补读。已经发生的外部动作由写入记录核查，不用观察者落盘来保证执行前顺序。

## 7. 配置设计

### 7.1 配置只保存具体值

```rust,ignore
struct Profile {
    id: ProfileId,
    label: String,
    endpoint: bone_llm::EndpointConfig,
}

struct ModelSelection {
    profile: ProfileId,
    model: String,
    options: Option<bone_llm::ModelOptions>,
}

struct RuntimeSettings {
    worker: Option<ModelSelection>,
    coordinator: Option<ModelSelection>,
    limits: bone_agent::AgentLimits,
    tools: ToolSettings,
}

struct RuntimeOverrides {
    worker: Option<ModelSelection>,
    coordinator: Option<ModelSelection>,
    limits: Option<bone_agent::AgentLimits>,
    tools: Option<ToolSettings>,
}

struct ToolSettings {
    mode: ToolMode,                 // ReadOnly / WorkspaceWrite
    limits: bone_tools::ToolLimits,
}
```

User 保存默认值，Workspace 和 Session 保存显式覆盖。按字段选最近的设置：Session > Workspace > User。`limits`、`tools` 作为完整组替换，不做递归 JSON merge 或每个数值的多层继承。

`None` 在覆盖中表示继承。Coordinator 各层都未指定时，跟随最终解析出的 Worker；Worker 缺失则返回 NeedsModel。只读模式是默认值，启用 WorkspaceWrite 是明确的工具配置变化。

默认限额来自 AgentLimits 和 ToolLimits 自己的 Default。给 AgentLimits 补序列化支持，App 不维护另一份逐字段镜像。

配置修改用一个固定 `ConfigChange` enum 表达，例如 SetWorker、SetCoordinator、SetAgentLimits、SetTools。SetWorker/SetCoordinator 接收 Option<ModelSelection>，None 清除该层选择并恢复继承；Workspace/Session 的整组限额与工具覆盖同样可以清除。普通字段修改按提交顺序处理，不需要前端读整份配置再覆盖，也不需要任意字符串路径、配置描述器注册表或 schema 插件系统。

### 7.2 有效值和运行中值

```rust,ignore
struct ResolvedConfig {
    desired: Result<RuntimeConfig, ConfigProblem>,
    sources: ConfigSources,
    running: Option<RuntimeConfig>,
}
```

RuntimeConfig 包含已确定的两种模型、工具配置、AgentLimits 和 Workspace 路径。它没有 secret，在创建 Runtime 时保存一份；记录配置本身，不只保存一个无法还原的 hash。首次尚未选模型可以成功保存配置，desired 用 ConfigProblem::NeedsModel 表达未完整，不阻止打开 App。

保存配置成功后返回新 desired 值。当前 Runtime 继续使用 running 值。用户关闭 Runtime 后再次执行，采用新配置并从历史构造启动背景。第一版不提供自动重启、模型热切换或工具热插拔。

角色调用超时由 AgentLimits 决定，删除旧 ModelSelection 中的另一套任务 timeout。Bash 的进程执行期限仍属于 ToolLimits，运行时的 tool_timeout 应给它的退出清理留出时间；二者是不同执行边界，解析时核对有效关系。

## 8. 模型与工具装配

### 8.1 启动步骤

SessionTask 在需要创建 Runtime 时执行一个普通的具体装配函数：

```text
解析并固定 RuntimeConfig
  → 获取已登录的凭据，连接 Coordinator / Worker
  → 构造 Workspace ToolEnvironment
  → 安装 read/glob/grep + session_history
  → 按配置安装 apply_patch/bash
  → 构造有预算的 Session 历史背景
  → 保存 RuntimeId 与配置
  → Agent::with_ports(...)
  → 建立观察，再按顺序投递已保存输入
```

连接操作不持有 Session 命令锁；完成结果回到 SessionTask 后再核对是否仍需要启动。一个 Session 同时只有一次启动操作，多个排队输入共享它。

沿用 `ModelAdapter` 的 coordinate/work/compact 协议，App 不复制 submit_work 或 submit_coordination schema。公开现有 ModelAdapter 构造和只读工具适配入口后，App 可以直接组合。

### 8.2 工具组成

| 工具 | 效果分类 | 装配方式 |
| --- | --- | --- |
| read / glob / grep | ReadOnly | 复用 bone-tools 和已有适配器 |
| session_history | ReadOnly | 具体 App ToolPort，分页读取本 Session 已保存历史 |
| apply_patch | ExternalWrite | 具体 PatchPort，理解补丁执行及回滚结果 |
| bash | ExternalWrite | 具体 BashPort，记录命令实际结束状态 |

任意 Bash 命令都按 ExternalWrite 处理，不解析命令字符串猜测它是否只读。Bash 退出码不为零也可能已修改文件；退出码与副作用是否确定是两个字段。

同一个 App 中，共享 Workspace 的 Patch/Bash 适配器共用一个 `Mutex<()>`，一次只执行一个写调用。读工具继续并发。这个锁保护单次工具执行，不保证“之前读到的文件”到“后来写入”之间无人修改；Patch 自己的上下文匹配仍负责拒绝不匹配的修改。

第一版不自动创建 worktree，也不宣称不同 Session 拥有隔离的文件系统。需要隔离的任务使用不同 Workspace。跨进程共享 Workspace 的写隔离不在这一把进程内锁的保证范围内。

### 8.3 写调用记录

异步观察 CallStarted 无法保证先落盘再执行。写工具适配器在真正执行处记录一个具体 `WriteAttempt`：

```rust,ignore
struct WriteAttempt {
    call: CallRef,
    session: SessionId,
    tool: String,
    arguments: serde_json::Value,
    state: WriteState,              // Pending / Finished(ToolOutcome)
}
```

顺序固定：取得 Workspace 写锁，检查同 Workspace 没有未决写，确认仍应执行，保存 Pending，调用工具，保存结果，再向 Agent 返回结果。Pending 保存失败就不调用工具。结果保存失败则该次写仍需核查，不把它当成确定未执行。

锁与 WriteAttempt 由实际工具执行任务持有到 commit、rollback 或进程清理结束；不能仅由可能被 Agent timeout 丢弃的外层 Future 持有。已有 Patch 事务会在外层 Future 被取消后继续收尾，适配必须覆盖这段真实执行期。

同 Workspace 有 Pending/Unknown 写时，新的写调用返回待核查原因，只读检查仍可执行。这个检查放在同一把写锁内，避免关闭旧 Runtime 再新建一个 Runtime 就绕过未决写。它不增加审批操作；核查后 resolve_write 更新事实即可放行。

只有能够确认发生在执行前的失败返回 ExternalEffect::None。已经正常结束的调用记录其已知结果；无法确认效果的取消、超时或执行中异常返回 Unknown。Patch 的回滚结果由具体适配器或工具的类型化结果提供，不解析错误字符串。

这是 App 工具模块里的具体逻辑，不增加 middleware 管线、通用事务协调器或 exactly-once 框架。工具适配器只写属于该 Call 的 WriteAttempt；会话 metadata、输入和 journal 仍由 SessionTask 写入。

调用方通过 resolve_write 提交核查结论及依据。活着的旧 Runtime 用其 CallRef 对应 Agent resolve_write；已经关闭的 Runtime 只更新持久事实，不把旧 CallId 发给新 Agent。一次核查不自动重发工具，也不恢复旧 Job。

## 9. 登录与连接

登录是 App 能力。前端负责显示 challenge 和用户交互，App 负责授权操作的生命周期。

`LoginAttempt` 是一个短期具体句柄，提供 `watch<LoginState>` 和 cancel。状态包括 Connecting、DeviceCode、Succeeded、Failed、Cancelled。DeviceCode 只放在这段临时状态中，不进入普通 Session 历史。

运行时启动只使用已有授权，授权不可用时返回 LoginRequired。显式 login 才允许交互式 device flow。现有 `bone-llm` ChatGPT connect 会允许 device flow，需要补充非交互连接入口。

API key 继续由系统 credential manager 保存。Profile 和 RuntimeConfig 不包含 key。每个新 Runtime 按当前 profile/key 构造 API-key endpoint。

同一 App 的多个 ChatGPT Session 共享一个有效的 ChatGPT endpoint/credential lease；现有 lease 是可克隆对象，不应为每个 Session 再取得一把相同的独占锁。连接的建立与释放由 providers 模块直接管理，不建通用连接池。Logout 先释放 App 自己的闲置连接；仍有 Runtime 使用时返回 Busy。

## 10. 持久化与重启

### 10.1 保持一个数据库

将现有 BoneStore 的 SQLite、typed Document、Journal、短事务和 lease 实现内移复用。由私有 storage 模块定义 key、业务记录和原子写方法，前端不接触它们。

保存以下数据：

| 数据 | 内容 |
| --- | --- |
| Workspace / Profile / Config | 身份、路径、模型目录和配置 |
| SessionRecord | 标题、草稿、归档、输入计数与最近应用投影 |
| SavedInput | RequestId 去重、输入正文、InputId、投递与完成状态 |
| RuntimeRecord | RuntimeId、配置快照、启动和关闭事实 |
| Session journal | App 输入/生命周期事实，以及带 RuntimeId 的 Agent 记录 |
| WriteAttempt | 写工具执行前的意图和后续已知结果 |

Session 的 journal 只有一份。内部可用 `SavedEvent::Agent { runtime, record }` 保存原始 Agent 事实，公开 history 时转换成 Session 历史条目；不再落盘一份重复的“前端事件日志”。工作笔记、Checkpoint、Delivery 等内部事实不默认出现在用户时间线，证据读取仍能定位其内容。

高频 CallProgress 只更新 View。其余事实按 `(RuntimeId, AgentSeq)` 归档去重，并在同一事务中更新应用投影和已保存水位。必要的来源索引由存储模块维护，可从 journal 重建；它不是另一份内容来源。

公开 history 可能过滤掉内部事实，因此 SessionSeq 允许在返回条目之间有间隔。`next_cursor` 表示已经扫描到的位置，即使某页没有可见条目也必须推进；客户端以 has_more 继续读取，不能用最后一个可见条目的序号替代扫描水位。

现有存储只有全量 Journal::read，内移时补真正的 `read_after(after, limit)` 数据库分页，并支持按序号读取用于证据定位。证据分页按 UTF-8 边界返回内容与下一 offset，history 列表不携带整块工具输出。

当前 store 单条 journal 上限为 1 MiB，而 Agent 默认工具输出也可到 1 MiB，包装和转义后可能超限。实施时必须统一这两个实际边界：扩大可保存的记录上限，并验证配置及实际编码大小；不能通过静默截断证据来让存储成功。

### 10.2 能恢复的内容

正常退出时：关闭接收新命令，结束或取消当前工作，等待 shutdown grace，最后归档 Agent 尾部事实和 ShutdownReport，再释放 Session writer。

为使最后一批记录可可靠取得，Agent shutdown 的返回值增加冻结的最终 AgentView。App 在关闭期间正常消费观察，最后再用该 View 补齐到末尾。仅排空 broadcast 不足够：批量 Stop 可能产生超过 256 条记录，而 Actor 退出后已无法重新 observe。这是一个具体的终态读取入口，不增加常驻归档服务。

冷启动取得 Session writer 后：

- 打开已保存输入、历史、配置和最近应用投影。
- 上次 Runtime 没有正常收尾的非终态输入标为 Interrupted。
- 未完成的 WriteAttempt 显示为待核查，不自动重发。
- 只读浏览保持 Runtime Detached；新工作才创建新 Agent。

Queued 输入若能从持久投递阶段证明从未发往 Agent，保留为可显式 retry；一旦进入“准备投递”阶段却没有可靠确认，就按 Interrupted 处理。需要在调用 Agent post 前保存这个阶段，不能用“没有 InputAccepted 日志”推断 Agent 从未收到。

第一版不恢复 Kernel 的 Job DAG、Future 或精确执行位置。持久化的应用投影只描述最后已知状态，不能作为 Kernel restore 数据。

### 10.3 新 Agent 如何理解旧会话

一个可使用的 Session 需要让新 Runtime 读到旧历史。当前 Agent 构造接口没有历史入口，这项必须明确补齐。

启动时 App 从已保存历史中选取近期用户输入、公开回复、root 最终结果和中断/未决事项，生成一段结构化历史背景。先采用确定性选择，不增加专门的摘要模型或 memory service。

背景遵守 Agent context_bytes 的预算，按完整条目选择，保留来源和历史范围，明确标注被省略的部分。具体预算从上下文上限分配，必须为本次输入和工作留出空间；大正文通过 session_history 按需读取。

Agent 增加一个构造时的只读 background 字段，把它作为历史资料纳入 CoordinateInput / WorkInput，并计入同一个上下文预算。它不进入当前 Job/Input/Call ID 空间，不伪造旧工具结果为当前执行事实。ModelAdapter 对其使用外部资料通道，历史中的动作要求不会自动变成新任务。

用户重启后说“继续检查昨天的问题”，产生新 Input；Agent 参考背景并按需读历史，再决定接下来做什么。它可以重新检查文件和运行测试，但不会仅凭旧历史自动重复一项发布或修改。

## 11. 代表性调用流程

### 11.1 首次使用，尚未选择模型

1. 前端打开 App 和 Workspace，创建 Session，立即能保存草稿和读取历史。
2. 用户提交文本，App 保存输入并返回回执；View 显示 Queued / NeedsModel。
3. 用户通过配置 API 选择模型，必要时通过 login 完成授权。
4. 调用 retry，App 装配 Runtime，投递同一个尚未发送的 Input。
5. 前端从 history 获取回复，从 View 获取进度，直到自己的输入终态。

Shell 的存在与模型是否连接无关；这里不需要前端实现持久接受或 Runtime 启动逻辑。

### 11.2 正在查两个问题，用户补充范围

用户先提交“检查登录超时和连接池配置”，再补充“只看生产环境”。两次 submit 得到不同 InputId。App 按顺序交给同一 Agent，由 Coordinator 处理范围变化。

App 不尝试取消所有旧 Job，也不在外层重新拆任务。两个输入分别更新自己的状态，子 Job Outcome 不提前结束任一输入。

### 11.3 TUI 和另一个客户端同时打开会话

两个客户端取得同一 Session，分别 observe，并各自保存 history cursor。TUI 断开后，SessionTask 继续工作。另一个客户端仍能收到状态，TUI 重连后从自己的 cursor 补读。

如果两边都回答同一问题，第一条有效回答被 Agent 接受后该 QuestionId 失效。第二条在保存前被发现无效就返回 InvalidState；若已经保存，投递时再失效则通过 InputRejected 结算。它不会被当普通补充输入投递。

### 11.4 改模型

执行中保存新 Worker 配置，resolved_config 同时返回新 desired 和旧 running。当前工作继续使用旧模型。用户 close_runtime 后再次提交，新 Runtime 使用新模型和有界历史背景。

前端无需自行销毁 Agent，也不用拼接聊天记录实现上下文迁移。

### 11.5 写文件时进程退出

PatchPort 已保存 WriteAttempt(Pending)，随后开始改文件。如果进程在结果落盘前退出，重启时能够指出哪次调用、哪个工具、什么参数仍未核实。

用户或宿主核查文件后通过 resolve_write 保存结论。旧输入保持 Interrupted；用户的新指令决定是否继续执行后续测试。这个流程只核实已有副作用，不重放旧调用。

### 11.6 one-shot

one-shot 同样创建或打开一个 Session，submit 后按 InputId 等待。收到 InputFinished 返回对应退出结果；需要用户回答、路由失败或输入被拒绝时返回明确状态及 SessionId。历史仍可由其他前端打开，不再维护与交互模式不同的临时执行协议。

## 12. 代码组织与重写顺序

目标目录保持平直：

```text
crates/bone-app/src/
├── lib.rs          公共类型与入口
├── api.rs          请求、回执、View、历史 DTO
├── app.rs          依赖所有权和 Session 注册表
├── session.rs      会话任务、提交、观察、关闭
├── config.rs       typed 配置及解析
├── providers.rs    模型连接与显式登录
├── credentials/    复用具体凭据实现
├── tools.rs        工具装配、历史读取、写入记录
└── storage/        原 bone-store 内移，连同 App 业务存储
    ├── mod.rs      私有 Store 与具体业务方法
    ├── sqlite.rs   连接与短事务
    ├── journal.rs  记录、分页和来源定位
    ├── schema.rs   schema 与数据迁移
    └── ...         复用的 document / lease / 路径与测试

crates/bone-tui/src/
├── main.rs         bone 二进制与启动
├── app.rs          前端交互状态
├── render.rs       展示
└── ...             输入与终端适配
```

函数增多后可按明确职责拆文件，但不预先增加 domain/application/infrastructure 三套同名对象。

### 12.1 必要的上游改动

| Crate | 改动 |
| --- | --- |
| bone-agent | 公开 ModelAdapter 与只读工具适配入口 |
| bone-agent | AgentLimits 序列化；构造时历史 background 与统一预算 |
| bone-agent | 控制返回实际结果；回答时原子核对问题 Seq；shutdown 返回最终 View |
| App 内部 storage | 从 bone-store 内移；补 journal 分页、定位读取和记录大小约束 |
| bone-llm | ChatGPT 非交互连接入口，显式登录保留 device flow |
| bone-tools | Patch/Bash 需要时补具体执行效果信息，避免靠错误字符串判定 |

这些改动服务于实际装配，不重做 Agent 调度和上下文内核。

### 12.2 实施顺序

1. 固定本文的 API 和数据契约，建立 headless App + Session，内移 bone-store，移走终端依赖。
2. 接通配置、凭据和只读 Agent，完成保存输入 → 执行 → history/view 的无前端闭环。
3. 完成多输入关联、问题回答、stop 顺序、配置生效和历史背景。
4. 接入具体写工具、WriteAttempt 与关闭/中断核查。
5. TUI 和 one-shot 统一改为 App 客户端，删除旧 AgentHost/Handle/Notice 转换代码。

每一步用实际 API 场景验收，不先建立一套无人调用的通用服务层。

### 12.3 数据兼容

代码和公开 Rust API 可以破坏性重写；已有用户数据库不因此被清空。实施时为新 App 记录使用明确的新格式标记或命名空间，给旧设置与会话历史做一次性显式转换。转换成功后使用新格式，旧记录保留到转换验收结束。

不维护长期运行的两套 App 模型或双写协议。旧记录中缺失的 Runtime/Input 关联保留为旧历史，不凭空补出执行关系。

## 13. 验收与规模约束

重点测试应用边界，不复制 Kernel 测试：

1. 没有 TUI，直接通过 App API 完成一次输入和结果获取。
2. 相同 RequestId 重试不产生第二条输入；取消调用方等待不撤销已保存提交。
3. 多 Input、多 Job 的回复和终态关联正确；旧问题的回答被拒绝。
4. stop 之后没有此前尚未投递的输入突然启动。
5. 两个客户端观察同一 Session；慢消费者与重连能完整分页读取历史。
6. 配置改变只作用于新 Runtime；新 Runtime 可读历史且不重放旧动作。
7. 同一 App 两个 ChatGPT Session 能共享凭据连接。
8. 写入意图先于实际工具调用落盘；结果不确定时可定位、核查。
9. 正常关闭保存最后结果；冷启动区分未投递、Interrupted 和未决写。
10. 对实际序列化上限、分页游标和一次事务失败做边界验证。

测试依赖现有 ModelPort/ToolPort 注入与临时数据目录，原 bone-store 的事务、分页和跨进程 lease 测试随实现迁入 App；不为测试建立 AppApi、Repository 或 ProviderFactory trait。

第一版不加入分布式执行、通用插件容器、工作流引擎、审批框架、动态配置 schema、自动补偿系统、模型热更新和自动执行恢复。也不主动加入一套端口、服务、适配器同名转发层。每个新增类型都应有独立的数据含义、所有权或调用职责。

## 14. 当前代码依据

- [Agent 输入、控制和观察](../crates/bone-agent/src/runtime.rs)：公开 Agent 句柄、快照与 broadcast 的真实边界。
- [Input / ModelPort / ToolPort](../crates/bone-agent/src/ports.rs)：当前输入为文本，端口支持取消和进度。
- [Record 定义](../crates/bone-agent/src/context.rs)：Reply、Outcome、InputFinished 的不同语义。
- [现有模型装配](../crates/bone-agent/src/app.rs) 与 [导出范围](../crates/bone-agent/src/lib.rs)：默认只读工具及适配器公开性缺口。
- [配置旧实现](../crates/bone-app/src/settings.rs)、[Provider 连接](../crates/bone-app/src/providers.rs)：可复用机制及待删除的旧 Agent 类型。
- [旧 Session 控制](../crates/bone-app/src/tui/session_controller.rs)：当前藏在 TUI 内的应用逻辑，应迁入 SessionTask。
- [Store 事务](../crates/bone-store/src/lib.rs)、[Journal](../crates/bone-store/src/journal.rs)：已有持久化能力及分页缺口。

本次只完成设计文档。当前 bone-app 仍未迁移到新 Agent API；此前库编译检查已确认旧类型引用失败，本文不把目标结构写成已实现行为。
