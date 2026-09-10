# BONE：轻量意图路由与可纠正 Worker

日期：2026-09-09。状态：调研与设计建议，未实现、未运行模型对照实验。

本轮使用三个并行 agent，分别研究论文与路由模式、Agent OS 与框架、BONE 本地契约。根 agent 复核关键来源并综合。本文的目标方向取代旧 context-engine HTML 中“协调模型顶层规划、App 负责 Session 摘要”的假设，但 HTML 尚未同步更新。

## 结论

轻量 Router 把请求交给负责的 Worker，Worker 调查、规划、执行并可纠正路由，在工程上可行。规划并未取消，而是从全局入口移到每个需求的负责人。异构模型、handoff、后台记忆维护都有先例；现有来源不证明某个指定轻量型号足以完成 BONE 的多轮路由，也不证明成本和延迟必然改善。

有价值的研究问题是：在多轮、有状态、有副作用的任务中，可纠正的路由协议能否让轻量入口达到接近强协调者的任务质量，并减少协调开销？不能据本轮检索声称全新算法。

## 一手证据

| 来源 | 确认的内容 | 证据边界 |
| --- | --- | --- |
| [Anthropic — Building effective agents](https://www.anthropic.com/engineering/building-effective-agents) | 区分 Routing 与 Orchestrator-workers，模式可以组合。 | 不证明轻路由总优于强规划者。 |
| [Microsoft Agent Framework — Handoff](https://learn.microsoft.com/en-us/agent-framework/workflows/orchestrations/handoff) | 官方 triage 只路由，专家可以交回 triage；区分任务转交与子任务委派。 | 是机制示例，不是质量或成本实证。 |
| [AutoGen Swarm](https://microsoft.github.io/autogen/0.7.3/user-guide/agentchat-user-guide/swarm.html) | agent 本地决定交接。 | 默认共享消息上下文，不宜照搬为 BONE 的 Job 权限模型。 |
| [RouteLLM，ICLR 2025](https://arxiv.org/abs/2406.18665) | 基于偏好学习强弱模型选择，在论文基准上体现成本与质量折中。 | 模型选择不同于判断长期会话里的 Job 归属，其收益数字不可移植。 |
| [Arch-Router](https://arxiv.org/abs/2506.16655) | 1.5B 路由模型学习匹配领域、动作与用户偏好。 | 不评测 BONE 的任务所有权、恢复或副作用。 |
| [AIOS，COLM 2025](https://arxiv.org/html/2403.16971v5) / [官方仓库](https://github.com/agiresearch/AIOS) | 将调度、上下文、记忆、存储、工具等组织成内核服务；包含 FIFO/RR 调度。 | AIOS kernel 不等于轻量意图模型，资源调度吞吐不能证明语义路由收益。 |
| [Letta — Memory & dreaming](https://docs.letta.com/configuration/memory) | 后台 subagent 整理记忆，为独立记忆 Worker 提供先例。 | 摘要准确性、范围、版本提交仍需 BONE 定义。 |
| [MemGPT](https://arxiv.org/abs/2310.08560) | 层次记忆与有限上下文管理。 | 不直接检验轻路由与 Worker 纠错。 |
| [Magentic-One](https://www.microsoft.com/en-us/research/articles/magentic-one-a-generalist-multi-agent-system-for-solving-complex-tasks/) | 重 Orchestrator 负责规划与跟踪。 | 是有效对照架构，不是必须复制的组织形式。 |
| [CRITIC，ICLR 2024](https://arxiv.org/abs/2305.11738) | 外部工具反馈帮助验证和纠正输出。 | 不证明 Worker 的反对意见天然正确。 |
| [Large Language Models Cannot Self-Correct Reasoning Yet](https://arxiv.org/abs/2310.01798) | 所研究模型/任务中，无外部反馈的自我纠错可能失败或退化。 | 不是针对现代强 Worker 纠正弱 Router 的直接实验，也不是对所有纠错的否定。 |
| [Why Do Multi-Agent LLM Systems Fail?](https://arxiv.org/abs/2503.13657) | 系统设计、agent 失配、验证和终止是主要故障类别。 | 不能由此推断 BONE 失败率或否定全部多 agent 系统。 |

## 目标职责

| 角色 | 职责 | 不承担 |
| --- | --- | --- |
| Rust Kernel | 状态、权限、调用资格、调度、持久提交边界 | 证明自然语言解释正确 |
| Router 模型 | 新输入属于已有工作、新工作还是需要消歧；可给类别提示 | 技术任务拆分、改写用户目标、授权工具、直接改全局约束 |
| Owner Worker | 理解原始请求、调查、规划、执行、必要时委派、交付 | 擅自取得其他 Job 的私有记录或扩大权限 |
| 专门 Worker 调用 | 如摘要、审查，在专门输入输出契约内完成认知工作 | 默认获得完整 Job 的工具/委派权限 |
| Runtime | 执行模型、工具以及拟议的持久化 Effect，回传结果 | 自行决定语义归属 |

路由归属、模型配置、规划委派是三条独立轴。profile 指定模型和推理预算，不自动授予工具权限。root Worker 可以是强规划者；不强制每个 root 再创建 planner 子任务，也不强制简单问答拆成多个 Job。

## 最小路由协议（拟议形状）

```text
RouteProposal
  input_ids / routing_revision
  destination:
    existing_root(job_id)
    new_root
    unresolved(candidate_ids)
  profile_hint: configured_profile | unknown
  short_reason
```

这是单个交付决定的示意。多请求/多目标批次的原子性仍需另定；V1 不要求轻 Router 将一句复杂请求拆成多个精确任务。可先交给一个 generalist owner，再由它规划。

Router 读取用户原文、可靠 reply 引用、近期会话、活跃 root 的简短工作目录和必要共同约束。默认不给所有 Job 私有过程。目录不能只有标题，应包含目标摘要、当前等待事项与关联输入；目录不完整时允许补读或 unresolved。

原始输入必须独立保存、原样交付 Worker。路由理由是模型判断，不能以系统指令身份覆盖原话。

明确问题回答、指定 Job 的输入、工具结果和结构化控制走代码路径，不必每次调用 Router。自由文本“停止那个”仍需要确定目标；界面已有明确目标的 stop 应直接执行。

长会话要保留工作归属连续性，不能把每条“继续”“改一下”作为孤立文本重新分类。但不能把所有新话题一律交给当前 Job。

## 纠错协议

| Worker 发现的问题 | 处理 |
| --- | --- |
| 原来理解成实施，现在发现只是评审 | 同一 owner 调整理解与计划，若能力/权限合适，不必转交 |
| 缺少历史、证据或指代对象 | 请求必要上下文，不先归咎于路由 |
| 负责人正确，模型能力不足 | 请求换配置，保留 Job 身份、上下文和权限 |
| 输入确实属于另一项工作 | 提议转交该输入的处理责任 |
| 用户要求本身含糊或多个工作冲突 | 强 generalist Worker 消歧，仍不能判定时询问用户 |

```text
ChallengeRoute
  input_id / route_id / job_revision
  reason_code
  evidence_refs
  suggested_target（可缺省）
```

Kernel 校验提交来源、版本、目标、权限、次数与原子交接条件，不判断自然语言真伪。语义争议不能反复交回同一个弱模型；建议先试“最多一次自动转交，再升级强 generalist”的有界策略，阈值由实验调整。Worker 自报 confidence 不能作为可靠概率。

转交不同于子 Job 委派：转交改变该输入的处理责任；委派保留 parent 的交付责任。已有 Job 收到一条错投的新补充时，不应销毁整个旧 Job。

交接需保留原始输入、已发生动作和约束；已发出的工具调用不能因换负责人而被重放。交接提交后旧调用资格失效，但迟到工具结果仍是真实事实。Unknown 写入继续核查。所有权交接的 durable 协议还需与 Core 恢复设计闭合，不能仅用停止一个 Worker、启动另一个替代。

## Context 的直接影响

- Session 保留原始输入、公开结果、共同约束和有界公共历史摘要。
- Router 看有界公共目录与路由相关历史。
- Owner Worker 看原始请求、相关 Session 背景、自己的计划和工作记录。
- Child 只看明确交付的目标、记录与证据。
- 压缩调用仅看到批准的前缀，返回 draft。代码决定何时压缩、范围、预算、来源资格和提交；模型决定如何表达摘要。

“都是 Worker”应统一模型调用基础设施，而不把压缩强制做成可以再委派、执行工具的通用 Job，以免引入递归与多余生命周期。

Core-owned durable 是另一维度：Core 定义保存/恢复契约，外部实现存储。它与轻路由兼容，但轻路由对照实验不必等待整个 durable Core 完成。重启仍必须单独定义 pending routing、未完成调用、交接与外部写入的处置。

## BONE 现状与改动

本轮代码核对：

- `crates/bone-core/src/job.rs::KernelDecision`：当前 Apply 能创建/更新 Job 与约束，明显超过意图路由。
- `crates/bone-core/src/model_contract.rs`：协调 prompt 要求创建或更新工作。
- `crates/bone-core/src/kernel/routing.rs`：已有 Worker 发起 Coordinate，但权限限于自身工作树，不等于通用误路由纠正协议。
- `crates/bone-core/src/kernel/scheduler.rs`：已有压缩触发、调度与回执校验；协调调用串行。
- `crates/bone-adapters/src/agent/model.rs`：已有 coordinator / worker 分离；compact 已使用 worker 模型。
- `crates/bone-app/src/config.rs`：已有两类模型配置；尚无本文建议的多类别 Worker profile。

主要改动：收窄路由输出；避免新建 root 时强迫弱模型填写完整技术 JobSpec；明确 owner 对计划的修改权与用户要求不可覆盖；增加模型选择和结构化纠错；再纳入 durable 提交和恢复。

## 对照实验

| 组 | 结构 | 用途 |
| --- | --- | --- |
| A | 原始输入直接给强 generalist Worker，无模型路由 | 检验入口额外一跳是否值得 |
| B | 当前顶层协调规划 + Worker | 检验规划下放 |
| C | 轻 Router + 强 owner，无纠错 | 测量轻路由的代价 |
| D | C + 有界纠错 | 单独测量纠错收益 |
| E | D + 多类别 Worker profile | 测量异构模型配置收益 |

固定工具、权限、基础材料、可用模型和预算条件；记录实际所有调用成本，不只看入口。建议先建小型标注集与可执行任务，再扩展规模；多次重复并报告分布，不能仅用少量示例宣布优势。

样本：新请求、含糊继续、指定问题回答、旧工作补充、新话题、多目标输入、跨工作限制、审查/实施混淆、能力升级、长历史指代、故意误路由。恢复和 Unknown 写入场景可先用协议测试，再与 durable 实现集成。

指标：端到端任务完成质量、用户意图偏离、首个有用响应/行动的 P50/P95 延迟、总 tokens/成本、误路由恢复率、正确路由被误纠正率、转交次数、重复外部动作、约束保留情况。标签准确率仅为诊断指标。

先让 Worker 不知情地接收人为错投案例，独立验证纠错：它究竟发现问题，还是顺从错误标签？同时加入正确路由案例，避免纠错功能只是频繁拒单。

总成本约等于 Router 成本 + Worker 成本 + 纠错与重复工作的成本。Router 即使很快，也增加串行调用；只有替代了更昂贵的协调或提高整体效率，才有经济收益。对比无路由强 Worker 时尤其不能预设获益。

## 推荐决策

值得实现小规模实验，建议目标为“轻路由 + 强通用 owner + 有界纠错”，随后再加细分类 profile。保留强协调方案和无路由方案作为对照。不要把 Router 的错误变成不可撤回的计划、授权或原话替代物。

用户列举的轻量模型可作为候选，本轮没有实测其路由准确率、延迟或费用，因此不作具体优劣承诺。
