# BONE Context Engine 设计交接

> 日期：2026-09-09
>
> 状态：设计尚未定稿；现有 HTML 已被明确否决为主设计稿
>
> 目的：让下一轮可以直接从代码事实和已收敛的架构判断继续，不再重复前面的错误方向

## 1. 用户真正要解决的问题

BONE 的执行基座已经基本完成，现在缺少的是 Agent 的上下文工程设计，尤其是：

- 长 Session 如何避免上下文随历史无限膨胀；
- 哪些事实可以被压缩，哪些必须保留原文或显式引用；
- 每次新输入应看到哪个 durable 水位之前的历史；
- Runtime 内的 Job working set 与跨 Runtime 的 Session memory 如何隔离；
- crash、迟到结果和未知外部写入如何进入后续上下文，但不制造虚假恢复；
- Core DTO 的字节预算与 provider 最终 prompt/window 之间如何闭合。

用户允许修改现有架构和抽象，目标不是“最少改动”，而是最简单、最优雅、能长期维护的方案。但设计必须从现有代码出发，不能另造一套与 BONE 状态机并行的抽象宇宙。

用户对交付形式的要求也已经非常明确：需要的是可以审阅系统如何设计的静态 HTML 设计稿，不是调研报告、产品界面稿，也不是一张通用架构图。

## 2. 当前仓库已经具备的上下文机制

### 2.1 Core 已有完整的 Runtime / Job-local 上下文机制

不要重写这部分。`bone-core` 当前已经有：

- `Kernel.records: BTreeMap<Seq, Arc<Record>>`：Runtime 内权威事实正文只保存一份；
- `JobContext.records: VecDeque<Seq>`：Job 只持有获准读取的 Record 引用；
- `JobContext.read_through`：记录已经交给有效 Worker turn 的事实水位；
- `Checkpoint { job, revision, through, summary, evidence }`：某个 Runtime 中某个 Job 的私有工作记忆；
- `RecordView { source, offset, next_offset, content }`：大工具结果和显式 Record read 的分页表示；
- `prepare_work` / `prepare_compact`：在 DTO 超预算时，只压缩已经读过的旧前缀；
- revision / active call 门禁：旧压缩结果和迟到模型结果不能获得提交资格；
- evidence capability：公开结果只开放显式 evidence，不递归泄漏来源 Job 的全部上下文。

关键文件和符号：

- `crates/bone-core/src/context.rs`
  - `Record` / `RecordBody`
  - `BootstrapContext` / `BackgroundEntry`
  - `CoordinateInput` / `WorkInput` / `CompactInput`
  - `Checkpoint` / `CheckpointDraft`
  - `prepare_work`
  - `prepare_compact`
  - `expand_records` / `RecordView` paging
- `crates/bone-core/src/job.rs`
  - `Job`
  - `JobContext`
  - `JobState`
- `crates/bone-core/src/kernel/scheduler.rs`
  - `compact_finished`
  - `accept_checkpoint`
- `crates/bone-core/src/kernel/work.rs`
  - Worker proposal acceptance and `read_through` advancement
  - Job revision changes invalidate the old checkpoint
- `crates/bone-core/src/model_contract.rs`
  - provider-neutral instructions, serialized context, submission schema and exact decoder
- `crates/bone-core/src/ports.rs`
  - `ModelPort`
  - `Input`
  - `ExternalEffect`

现有测试已经锁定“只压缩已读前缀”的核心语义：

- `crates/bone-core/src/tests.rs`
  - `compaction_replaces_only_an_already_read_prefix`

### 2.2 App 已有正确的 durable Session journal 和写入安全边界

`bone-app` 已经持久化：

- Workspace / Session / Input / RequestId；
- `StoredEvent::App(SessionEvent)`；
- `StoredEvent::Agent { runtime, record }`；
- Runtime config snapshot 和归档水位；
- 外部写入的 Pending / Finished / resolution；
- public Session history。

关键文件和符号：

- `crates/bone-app/src/persistence.rs`
  - `SavedSession`
  - `SavedRuntime`
  - `StoredEvent`
  - `DataStore::accept_input`
  - `DataStore::save_agent_record`
  - `DataStore::recover_session`
  - `public_agent_event`
- `crates/bone-app/src/session.rs`
  - `SessionTask`
  - `ensure_runtime`
  - `deliver_queued`
  - `archive_snapshot`
- `crates/bone-app/src/storage/journal.rs`
  - contiguous append-only Session journal
- `crates/bone-app/src/tools.rs`
  - `background`
  - `SessionHistory`
  - `WriteTool`
  - unresolved write gate
- `crates/bone-app/src/api.rs`
  - `SessionEvent`
  - `InputState`
  - `RuntimeState`
  - `WriteResolution`

这条 journal 已经是 Session Context Engine 的权威事实来源。不要新增 `ContextLedger`、第二条 Event Log 或平行状态机。

## 3. 真实缺口在哪里

当前缺口集中在 `crates/bone-app/src/tools.rs::background()` 与 Core 的 runtime-global `Kernel.background`。

当前行为：

```text
SessionTask::ensure_runtime
  -> tools::background(store, session, context_bytes, pending)
  -> 从 SessionSeq(0) 线性扫描 public history
  -> 在 context_bytes / 4 内保留最近若干完整 event
  -> BootstrapContext { entries, omitted }
  -> Agent::with_ports_and_background(...)
  -> Kernel.background 在整个 Runtime 生命周期固定复用
```

这意味着：

1. 长 Session 每次新 Runtime 都从 journal 起点线性扫描；
2. `omitted: bool` 只表示“丢过内容”，不知道具体缺口范围；
3. 没有 Session-level checkpoint、coverage 或 CAS head；
4. 没有 unresolved-write safety overlay；
5. 更关键的是，一个持续运行数小时的 Runtime 收到后续输入时，仍然使用 Runtime 启动时的旧 background；
6. 当前输入没有绑定一个明确的 durable watermark；
7. Core 的 `context_bytes` 只覆盖序列化后的 Core DTO，不包含 Adapter 后加的 instructions、submission schema 和 provider 包装。

因此真正缺少的不是一个通用 `ContextManager`，而是：

```text
bone-app::session_context
  + 每次输入绑定的 durable ContextBasis
  + immutable SessionCheckpoint + CAS head
  + checkpoint / tail / unresolved-write overlay 的有界投影
  + App -> Core 的 per-input BootstrapContext attachment
```

## 4. 已收敛的架构判断

以下结论经过多轮对抗讨论后已基本收敛，下一轮除非发现代码反证，不建议重新打开。

### 4.1 两个压缩平面必须保持不同语义

#### Session checkpoint

- owner：`bone-app`；
- identity：`SessionId + SessionSeq`；
- source：durable public Session facts；
- survives Runtime rotation / process restart；
- 只提供只读认知连续性；
- 不恢复 Job、Call、routing、tool authority 或 active action；
- 使用 immutable checkpoint document + CAS head。

#### Job checkpoint

- owner：`bone-core`；
- identity：Runtime-local `JobId + revision + Seq`；
- source：该 Job 获准读取且已经读过的 Record 前缀；
- private working memory；
- 只在当前 Runtime 内有效；
- 不能自动提升为 Session checkpoint；
- 建议后续将公开名字从 `Checkpoint` 改为 `JobCheckpoint`，避免和 Session checkpoint 混淆。

两者字段可能相似，但生命周期、权限和证据空间完全不同。不要抽成统一 `ContextFrame`。

### 4.2 最小的 durable 新模块放在 App

建议新增一个私有模块：

```text
crates/bone-app/src/session_context.rs
```

不要新增 crate。不要把 SQLite、SessionSeq 或跨进程恢复引入 Core。

该模块负责：

- 从 durable Session journal 选择可进入背景的 public facts；
- 记录和读取 Session checkpoint；
- 构造一个输入绑定的 `ContextBasis`；
- 在预算内投影成现有 Core `BootstrapContext`；
- 把 unresolved writes 作为不可压缩的 live safety overlay；
- 调度低优先级 Session compaction，并通过 CAS 提交结果。

### 4.3 冻结 ContextBasis，不持久化完整 prompt

推荐的 App 私有持久化类型：

```rust
struct ContextBasis {
    through: SessionSeq,
    checkpoint_through: Option<SessionSeq>,
}

struct SessionCheckpoint {
    through: SessionSeq,
    summary: String,
    evidence: Vec<SessionSeq>,
}

struct ContextHead {
    through: Option<SessionSeq>,
}

struct StoredInput {
    view: InputView,
    #[serde(default)]
    context: ContextBasis,
}
```

checkpoint document 建议使用：

```text
app/session-context/<SessionId>/checkpoint/<SessionSeq>
```

head document 建议使用：

```text
app/session-context/<SessionId>/head
```

`Document<T>` 自带 revision，直接作为 CAS version；不需要新增 SQLite 表。

为什么不持久化完整 prompt / snapshot：它会形成 `O(inputs x context_budget)` 的重复数据，而且把 provider presentation 错当成 durable truth。

为什么不能只保存一个不断前移的 head：queued input 或 crash 后重投时，会被新 head 带入提交之后的事实，破坏输入的历史边界。

`ContextBasis` 是中点：只保存事实范围和当时可用的 immutable checkpoint。它冻结的是语义边界，不是 provider prompt 字节串。

### 4.4 当前输入的 durable watermark

`DataStore::accept_input` 已经在同一事务中追加 `InputSubmitted` 并保存输入，因此最自然的定义是：

```text
S = 当前 InputSubmitted 的 SessionSeq
W = S - 1
```

同一事务中应完成：

```text
read ContextHead H
append InputSubmitted at S
store Input + ContextBasis { through: W, checkpoint_through: H }
store RequestId index
commit
```

当前输入永远不进入自己的 background；它由 Core `Input` 单独携带，并保持最高优先级。

这条规则适用于普通输入和 `SubmitInput::answer`。

### 4.5 一次 Context build 的确定性规则

V1 不做 embedding、向量检索、语义 rank 或 branch merge。一次 build 只做：

```text
build(ContextBasis, current unresolved writes, budget)
  = immutable SessionCheckpoint(H)
  + chronological public SessionEvent tail (H, W]
  + explicit omitted range / cursor（若完整 tail 放不下）
  + live unresolved-write safety overlay
  -> BootstrapContext
```

约束：

- 只装入完整 event，不从中间静默截断；
- 装不下时必须给出显式 omitted range / cursor；
- 原始事实仍在 Session journal，可通过 `session_history` 显式读取；
- unresolved write 不进入摘要，不被 checkpoint 覆盖，不因 W 冻结而隐藏；它是当前安全状态 overlay；
- App 的 `SessionSeq` 不泄漏为 Core 权限；Core 仍只把 `BootstrapContext` 当 read-only history；
- build 失败不能改变 durable head 或输入事实。

### 4.6 Core 只做很小的 per-input attachment 扩展

推荐新增兼容 API：

```rust
Agent::post_with_context(input, background)
```

现有 `Agent::post(input)` 可暂时使用空 background 以减少迁移冲击。

最小传播路径：

```text
Agent::post_with_context
  -> InputCommand
  -> Kernel::accept
  -> InputEntry.background: Arc<BootstrapContext>
  -> Routing.background
  -> JobContext.background
  -> CoordinateInput.background / WorkInput.background
```

规则：

- 新 input routing 使用最新输入绑定的 background；
- 用户 reply 投递到已有 Job 时，把该 Job 的 background 更新为 reply 的 background；
- 新 child 继承 parent background；
- Job spec/revision 改变时仍沿用现有 call invalidation；
- 删除 `Kernel.background` / `with_ports_and_background` 的 runtime-global 语义；
- 不把完整 background 嵌入 `Input`，因为 `Input` 自己会进入 Record 和模型 DTO，嵌入会重复放大并混淆审计语义。

### 4.7 Session compaction 的提交协议

Session compaction 不是 Core Job，也不是一个万能 compiler。它是 App 的低优先级派生任务。

输入：

```text
expected head revision
previous SessionCheckpoint
完整 public SessionEvent prefix
target through Wc
```

输出建议保持最小：

```rust
struct SessionCheckpointDraft {
    summary: String,
    evidence: Vec<SessionSeq>,
}
```

提交前验证：

- summary 非空且有界；
- evidence 全部位于已提供的 durable source 范围；
- target `through` 单调前进；
- 未解决写没有进入 draft；
- draft 仍能在 Session context budget 内表示。

提交事务：

```text
write immutable checkpoint document
CAS ContextHead(expected revision -> new through)
```

CAS 失败时丢弃模型摘要，从新 head 重建；绝不合并两份模型摘要。模型失败或进程崩溃时 head 不变，原始 journal 不受影响。

Session compaction 可以复用当前 worker model selection，但不应把 `compact_session` 硬塞进 Core 的 `ModelPort`，否则 Core 会被迫知道 Session 语义。更合适的是 App / ProviderConnector 内部的一条窄 capability，使用 Adapter 已有的 strict structured-output request 机制。

### 4.8 Job -> Session 只能通过显式 public bridge

可作为 Session memory source 的内容来自 public `SessionEvent`，例如：

- `InputSubmitted`；
- `Reply`；
- `JobFinished`；
- `InputFinished`；
- 已有 policy 明确允许的 `ToolFinished`；
- `WriteResolved`。

不要直接压缩：

- `RecordBody::Note`；
- `RecordBody::Checkpoint`；
- 私有 routing read；
- 私有 Job Report；
- Worker scratchpad；
- old Job / Call authority。

`persistence.rs::public_agent_event` 已经是现有的显式提升边界，应沿用并加强，而不是绕过。

### 4.9 crash 和 unknown write 保持现有严格语义

V1 不恢复 active Job。

进程重启后：

- 已经进入旧 Runtime 但未终态的 Input -> `Interrupted`；
- in-flight model result -> 不再具备提交资格；
- old `JobRef` / `CallRef` -> stale；
- Session compaction task -> 可丢弃并从 durable head 重试；
- 用户“继续” -> 新 Input、新 ContextBasis、新 Job；
- 未解决外部写 -> `Unknown` / reconcile，绝不自动重放。

不要把 `Interrupted` 改名为 `Suspended`。`Suspended` 暗示同一 Job identity、owner tree、read cursor、effect state 和 action authority 都能恢复，而 V1 没有提供这种保证。

## 5. Provider budget 是相邻但独立的边界

当前真实链路：

```text
CoordinateInput / WorkInput / CompactInput
  -> bone_core::model_contract::ModelCall<T>
  -> bone_adapters::ModelAdapter
  -> Request { instructions, context JSON, submission schema, tool choice }
  -> provider protocol
```

`AgentLimits.context_bytes` 目前只约束 Core DTO 的 JSON byte size。Adapter 后续加入的 instructions、submission schema 和协议包装不在这个预算中。

因此设计稿中不能声称存在一个已经实现的“Exact Seal”。推荐的目标行为是：

- Core 继续做保守的 DTO byte admission；
- Adapter 在最终 Request 成形后执行 provider-aware admission；
- 超限在发网前成为明确的 `CallError`；
- Session Context Engine 只承诺自己的 `BootstrapContext` slice 有界，不接管 provider request ownership。

具体 token estimator / provider window catalog 尚未定稿，属于下一轮明确的 open decision。

## 6. 明确拒绝的方案

以下方案已讨论并认为不适合 V1：

- 新建统一 `ContextLedger` / Event Log；
- `ContextBranch` / DAG / multi-parent merge；
- 一个跨 Session、Runtime、Job、Provider 的统一 `ContextFrame`；
- 通用 `ContextCompiler` / `Digest<Scope>`；
- 把 Core Job checkpoint 直接持久化成 Session memory；
- 自动恢复 old Runtime 的 Job tree / Call / tool authority；
- V1 引入 embedding、向量数据库或语义 rank；
- 根据 checkpoint watermark 物理删除 Runtime Records 或 Session journal；
- 把 `ExternalEffect::Unknown` 总结成自然语言后继续执行；
- 在 Core DTO budget 通过后宣称 provider prompt 一定不会超限。

只有未来产品明确要求 active Job 跨进程或跨主机恢复时，才值得单独设计 durable workflow / branch architecture。那是另一个系统，不应偷偷混进 Context Engine V1。

## 7. 建议的实现顺序

### Slice 0：先重做设计稿

静态 HTML 必须以真实代码为主，不再使用通用架构图查看器作为整页骨架。建议页面结构：

1. Thesis：上下文是有界投影，不是 source of truth；
2. Reality：现有 Core / App / Adapter 已有什么、缺什么；
3. One Input：从 `InputSubmitted@S`、`W=S-1` 到 `WorkInput` 的完整 walkthrough；
4. Working Set：Job checkpoint 压缩前后，明确显示未读 tail、cursor、spec、constraints 和 unknown write 没被吞掉；
5. Contracts：实际 Rust 类型、目标类型、producer / consumer / non-meaning；
6. Budget：Core DTO 与 provider final request 的两层 admission；
7. Failure：Interrupted、late result、Unknown write、new Runtime；
8. Decisions：accepted / rejected / deferred；
9. Verification：每条不变量对应测试。

视觉语言应沿用 `design/tui-product-concept/index.html` 的 BONE 风格：深色、细边框、低圆角、mono 用于类型和状态、橙/青/绿/黄/红/紫具有固定语义。不要保留 Archify 的主题切换、雷达、图例、导出工具和 generic cloud/database/security 分类。

### Slice 1：ContextBasis 与旧数据迁移

- `InputView` 的持久化改为私有 `StoredInput`；
- `StoredInput.context` 使用 serde default；
- `accept_input` 同事务记录 `W=S-1` 和当时的 checkpoint；
- 旧 input 可用 `RequestRecord.saved_at - 1` 懒补 W；
- public App API 不暴露内部 ContextBasis。

### Slice 2：App Session Context build

- 新增 `bone-app/src/session_context.rs`；
- 用现有 document / journal 实现 ContextHead、immutable checkpoint 和 tail read；
- unresolved-write live overlay；
- 显式 omitted range / cursor；
- 删除 `tools::background()` 的职责，保留 `session_history` 作为显式 recall。

### Slice 3：Core per-input background

- 新增 `Agent::post_with_context`；
- background 从 InputEntry 传播到 Routing / JobContext；
- Coordinate / Work 使用对应 scope 的 background；
- 删除 runtime-global background；
- 添加 multi-input routing、reply 和 child inheritance 测试。

### Slice 4：Session checkpoint compaction

- App 内部 strict model contract；
- low-priority single-flight compaction；
- immutable checkpoint + CAS head；
- stale draft discard / retry；
- 不阻塞输入 durability；
- 不覆盖 unresolved writes。

### Slice 5：大历史分页与 provider admission

- `session_history` 对过大单 event 支持 `SessionSeq + UTF-8 offset` 分页，避免只能 skipped 后永远不可读；
- Adapter 在最终 provider Request 上做窗口 admission；
- 增加 provider-specific capacity contract tests。

### Slice 6：文档和物理 retention（后续）

- 更新 `docs/app.md`、`docs/core.md`、`docs/adapters.md`；
- V1 仍不删除 raw journal / Core records；
- GC 必须在引用闭包、evidence retention 和历史审计策略单独设计后再做。

## 8. 必须新增或保留的测试

建议最少覆盖：

- `input_context_basis_excludes_its_own_submitted_event`
- `queued_input_keeps_the_same_durable_watermark_after_restart`
- `new_input_in_a_live_runtime_receives_fresh_session_context`
- `merged_input_routing_uses_the_latest_input_basis_without_hiding_inputs`
- `reply_refreshes_the_target_job_background`
- `child_inherits_parent_background_without_new_session_authority`
- `session_checkpoint_compacts_only_public_durable_events`
- `session_checkpoint_never_imports_private_note_or_job_checkpoint`
- `session_checkpoint_cas_discards_a_stale_model_draft`
- `context_build_reports_the_exact_omitted_range`
- `unresolved_write_is_always_present_as_a_live_safety_overlay`
- `unknown_write_is_never_replayed_after_runtime_restart`
- `job_compaction_replaces_only_an_already_read_prefix`（保留现有语义）
- `job_checkpoint_does_not_imply_record_deletion`
- `session_history_pages_one_oversized_event_by_utf8_offset`
- `adapter_admission_counts_instructions_and_submission_schema`

## 9. 当前 HTML 产物的真实状态

仓库当前包含三类设计产物，全部一并提交是用户的明确要求，但它们不是等价的“已批准设计”。

### `docs/context-lifecycle-report.html`

最早的叙述型报告。用户已明确认为它不是设计稿。只作为过程记录保留。

### `design/context-lifecycle-concept/index.html`

当前是由 Archify 生成的 self-contained architecture viewer。它把 App / Core / Adapter 的部分结论画成 11 个节点，但用户已明确否决：它只是一张图，且通用 Archify 视觉语言不适合 BONE 的 Agent Context Engine 设计。

不要在它的当前结构上继续加卡片。下一轮应直接重建整页骨架。

### `design/context-lifecycle-concept/context-lifecycle.architecture.json`

Archify 的结构化源。它通过了 Archify 的 deterministic showcase validation：9/9 checks、0 errors、0 warnings。这个结果只说明 SVG/HTML 的结构与布局检查通过，不说明系统设计已被用户接受。

### `design/context-lifecycle-concept/index.visual-check.json`

自动浏览器验收没有成功完成。WSL 调用 Windows Chrome 时，DevTools `--remote-debugging-pipe` 出现 `ECONNRESET` / pipe file descriptor unavailable。该 receipt 的真实状态是 failed，不能宣称 browser evidence passed。

曾用 Windows Chrome 的普通 headless screenshot 成功渲染当前 HTML，确认页面能打开；截图位于系统临时目录，不在仓库内，也不能替代 Archify 的自动 browser evidence。

## 10. 回家后建议从哪里继续

第一步不要写 Rust。先把 `design/context-lifecycle-concept/index.html` 重建成真正的 Context Engine design review，并在页面里把所有未来类型标成 `PROPOSED`、现有类型标成 `CURRENT`。

设计稿首屏应直接表达：

```text
Context is a bounded projection, never the source of truth.

Durable Session facts  -> App owns
Live Runtime facts     -> Core owns
Job working set        -> Core owns
Final provider request -> Adapter owns
```

最值得做成可切换交互的是同一个 Input 的四种场景：

1. normal input；
2. tail over budget -> Session checkpoint；
3. crash after external write -> live Unknown overlay；
4. new Runtime -> new Input / new basis，不恢复 old Job authority。

每个字段的审阅器固定回答：

```text
它是什么
谁产生
谁消费
它不代表什么
哪条测试证明
```

这样页面是在审系统语义，不是在看颜色和箭头。

## 11. 最终一句话

BONE Context Engine V1 最简洁的形态是：

```text
一个 App-owned durable session_context
+ 现有 Core-owned JobContext / Record / Job checkpoint
+ Adapter-owned final provider request admission
```

新增的核心不是“更大的 Context 抽象”，而是一个输入级 durable 事实边界 `ContextBasis(W)`，以及两个永不混用的压缩生命周期。
