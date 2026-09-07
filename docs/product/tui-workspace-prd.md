# BONE TUI 配置、Workspace 与多 Session PRD

| 字段 | 内容 |
| --- | --- |
| 文档状态 | Ready for product & engineering review |
| 目标版本 | BONE TUI vNext |
| 产品优先级 | P0 |
| 最后更新 | 2026-09-07 |
| 产品范围 | 启动体验、TUI 配置、实时配置、Workspace、Session、Slash Command |
| 核心原则 | 配置文件是内部持久化格式，不是用户界面 |

## 1. 执行摘要

用户在任意目录执行 `bone` 后，必须直接进入一个可操作、可诊断、可恢复的 TUI。用户无需预先创建配置文件，无需复制示例 JSON，也无需知道模型 ID、配置 section 或配置文件路径。

启动时的精确当前目录，经规范化后成为本次 BONE 实例不可变的 Workspace。一个 Workspace 可以拥有多个相互独立、并发运行、可跨重启恢复的 Session。所有正式设置均可在 TUI 内通过选择器、表单或 slash command 完成，修改自动持久化，并在明确的安全边界实时生效。模型、推理强度和 Agent 行为在下一次用户轮次生效，不能在一个正在执行的用户任务中途换模型。

一句话产品定义：

> 精确启动目录定义不可变 Workspace；Workspace 包含多个持久化 Session；BONE 在零配置、未登录或配置异常时仍可进入 TUI；所有正式配置通过 TUI 完成并实时响应。

## 2. 背景与问题

### 2.1 当前已经具备的基础

BONE 当前已经拥有：

- 全屏、响应式、多 Session TUI；
- 一个 `AgentHost` 共享认证连接，多个 Session 独立运行；
- 每个 Session 独立的草稿、历史、任务、滚动位置与未读状态；
- 基于启动 `cwd` 的工具访问边界；
- 类型化配置、JSON Schema、字段校验、revision、文件锁与原子写入；
- 后台 Session 持续工作且不会抢走当前焦点。

### 2.2 当前用户问题

当前流程仍要求用户手动准备配置。交互模式会在进入 TUI 之前读取配置、要求 `agent.system`、完成认证、连接服务并创建第一个 Session。因此配置缺失、配置损坏、登录失败或模型不可用时，用户会在看到 TUI 前被阻断。

同时：

- 正常用户必须理解和编辑 JSON；
- 示例模型 ID 只是占位字符串，却可能通过本地校验；
- 登录信息显示在全屏 TUI 之外；
- 配置只在启动或 Session 创建时读取；
- 已运行 Session 不响应设置修改；
- 模型实例在 Session 创建时固定；
- slash command 只有硬编码的 `/stop` 和 `/exit`；
- Session ID 只在当前进程内有效，退出后无法恢复；
- 当前事件导出是观察日志，不是可靠的恢复存储。

因此，本项目不是“增加一份默认配置”和“补几个命令”，而是建立 BONE 的产品级 App Shell、响应式设置系统、Workspace 身份和持久化 Session 模型。

## 3. 产品愿景与体验原则

### 3.1 配置文件对普通用户隐形

- 正常用户从安装到长期使用都不需要打开配置文件。
- 不存在“只有手工编辑 JSON 才能完成”的公开设置。
- 配置路径、内部 key、schema 和 revision 只出现在高级诊断中。
- 手工配置可以保留为开发者逃生通道，但不进入正常文档路径。

### 3.2 TUI 始终是恢复入口

以下情况不能阻止 TUI 启动：

- 配置不存在；
- 配置部分无效；
- 配置文件整体损坏；
- 尚未登录或登录过期；
- 网络不可用；
- 模型列表暂时不可用；
- 第一个 Agent Session 尚未创建；
- 旧 Session 存储中有单条损坏记录。

### 3.3 默认值必须真实可用

- 不生成假模型、示例 token 或不可用 endpoint。
- 模型优先从当前账号的真实能力目录中选择。
- 内部默认值由代码或版本化 catalog 提供，而不是复制示例文件。
- 如果无法确定真实模型，进入 `NeedsModel`，不伪装成已就绪。

### 3.4 修改即保存，状态必须真实

- 设置项不需要“保存全部”按钮。
- 用户确认一个值后立即形成候选值并开始校验、准备、持久化和应用流程。
- UI 必须区分用户想保存的 `Desired`、当前运行时使用的 `Effective` 和可回退的 `Last Known Good`，不能把“已保存”写成“已应用”。
- UI 只有在运行时确认 `effective_revision` 后才能显示“已应用”。
- 应用失败时继续使用 Last Known Good，并给出重试、放弃候选值或查看详情的明确动作。
- 不能出现“界面显示新值，底层仍使用旧值”的分裂状态。

### 3.5 作用范围用人话表达

面向用户只使用：

- 仅当前对话；
- 当前工作目录；
- 所有工作目录。

内部可分别映射到 Session、Workspace、User scope。

### 3.6 实时不破坏进行中的工作

实时配置指无需退出、重启、新建 Session 或手工 reload。一次用户消息触发的完整 Agent 工作周期称为一个 User Turn。Turn 开始时锁定一份 `TurnConfig`；模型、推理强度、Coordinator 和 Agent 行为设置在整个 Turn 内保持一致。Turn 执行期间修改这些设置时，立即保存并显示为“下一条用户消息生效”，从下一条用户消息启动的新 Turn 起使用。展示设置即时生效，权限收紧可作为安全例外立即约束当前 Turn 后续尚未开始的工具调度。

### 3.7 不丢用户工作

认证、连接、配置写入、Session 打开或进程异常时，草稿、已确认提交的消息和旧的有效设置不能静默丢失。

## 4. 目标与非目标

### 4.1 产品目标

| ID | 目标 |
| --- | --- |
| G-01 | 用户第一次执行 `bone` 即进入 TUI，并在 TUI 内完成首次设置 |
| G-02 | 所有正式公开设置都能通过 TUI 完成 |
| G-03 | 设置修改自动保存并在安全边界实时生效 |
| G-04 | 启动的精确 `cwd` 成为稳定、不可变的 Workspace |
| G-05 | 一个 Workspace 支持多个独立、持久、可恢复的 Session |
| G-06 | slash command 可发现、可补全、可本地确定性执行 |
| G-07 | 配置、认证、连接与模型错误可在 TUI 内恢复 |
| G-08 | 普通错误不暴露内部 Rust、Provider 或配置实现细节 |

### 4.2 本版本非目标

- 将手工编辑 JSON 作为正式产品路径；
- 自动在每个项目目录创建 `.bone/`；
- 用 Git root 替代用户执行 `bone` 的目录；
- 在当前实例中通过 `/cd` 改变 Workspace；
- 静默迁移或重新绑定 Session 到其他 Workspace；
- 云端同步或跨设备同步 Session；
- BONE 进程退出后在后台继续执行任务；
- 对已经发出的模型请求热替换模型；
- 自动重放状态不确定的外部副作用；
- 第一阶段实现或在 TUI 中切换其他 Provider、自定义 endpoint；P0 仅覆盖当前 ChatGPT subscription Provider 内的认证、重连和模型选择；
- 在当前 subscription credential/权限模型完成认证前承诺 Native Windows GA；P0 支持 Linux、macOS 和 WSL2 的 Linux 命名空间；
- 第一阶段自动为每个 Session 创建 Git worktree；
- 第一阶段提供可提交到仓库的团队共享配置；
- 为尚未注册的 shell、patch 等能力展示虚假的可用设置。

## 5. 用户与 Jobs to be Done

### 5.1 首次使用者

> 当我第一次在一个项目里运行 BONE 时，我希望直接看到界面并按提示完成登录，以便尽快发出第一个任务，而不是先学习配置文件。

### 5.2 日常多任务开发者

> 当我在同一个目录并行处理多个问题时，我希望每个对话独立运行、独立保留草稿和模型选择，同时都明确绑定到这个目录。

### 5.3 回到旧项目的开发者

> 当我重新进入一个工作目录时，我希望看到这个目录以前的 Session，并从中断处继续，而不会混入其他目录的上下文。

### 5.4 高级用户

> 当我针对某个任务切换模型、推理强度或工具策略时，我希望全键盘完成，并准确知道影响范围、何时生效、是否已持久化。

### 5.5 遇到故障的用户

> 当登录、网络、模型或设置出现问题时，我希望仍能进入 TUI，知道数据是否安全，并在界面中完成恢复。

## 6. 核心概念与产品边界

### 6.1 Workspace

Workspace 是用户执行 `bone` 时的当前目录，经平台安全规范化后的产品实体：

```text
Workspace.root = canonicalize(process.cwd at launch)
```

规则：

- 一个 BONE 进程只有一个 Workspace；
- Workspace 在进程生命周期内不可改变；
- 不自动向上寻找 Git root；
- 同一 Git 仓库内的不同启动子目录是不同 Workspace；
- 指向同一真实目录的符号链接应解析为同一 Workspace；
- UI 可保留用户熟悉的 display path，安全边界使用 canonical path；
- Git root、remote、branch 和 worktree 只是附加元数据；
- 目录移动或重命名的自动迁移不属于第一阶段。

Workspace 的本地持久身份不使用可逆猜测的裸路径哈希。用户数据目录中的 `WorkspaceRegistry` 维护：

```text
CanonicalWorkspaceKey → random workspace UUID
```

首次遇到 canonical key 时生成 UUID，以后从同一 canonical key 解析到原 UUID。遥测标识与本地 Workspace ID 分离；遥测只能使用每安装实例随机 salt 派生的匿名值，不能上报本地 UUID 或路径哈希。

平台规范化是产品契约的一部分：

- Unix 解析 `.`、`..` 和符号链接，保留大小写语义；
- Windows 统一盘符大小写、路径分隔符和长路径形式，并按文件系统语义处理大小写；
- UNC、junction 与 symlink 指向同一真实目录时应得到同一 canonical key；
- WSL 路径与 Windows 路径默认属于不同平台命名空间，不做猜测式合并；
- canonicalize 失败时不退回父目录，TUI 进入可恢复的 Workspace 错误状态。

P0 平台矩阵：

| 平台 | P0 状态 | 说明 |
| --- | --- | --- |
| Linux | 必须认证 | 原生 Unix 路径与权限语义 |
| macOS | 必须认证 | Unix 路径、symlink 与 credential 权限语义 |
| WSL2 | 必须认证 | 视为 Linux 平台命名空间；Windows Terminal 只是终端宿主 |
| Native Windows | P1 | Workspace 规范已预定义，但需独立完成 credential、ACL、junction/UNC 与终端认证后才能宣称支持 |

### 6.2 Session

Session 是绑定到一个 Workspace 的持久化对话与执行上下文。Session 创建后不能修改 `workspace_id`。产品必须区分逻辑 Session 与进程内 Runtime：

```text
SessionRecord                         Agent Runtime
├── 持久 ID                           ├── 进程内对象
├── Workspace 绑定                    ├── 按需创建、按需释放
├── 草稿、消息、历史                  └── 不跨进程恢复
├── 配置覆盖与恢复数据
└── 不依赖登录或模型即可创建
```

首次启动时可以立即创建 `SessionRecord` 并保存草稿或排队消息；认证、模型和连接就绪后才惰性附着 Agent Runtime。重新启动时先加载 Session 索引，只有被选中、收到新消息或确有后台工作的 Session 才 hydrate 历史并附着 Runtime，不能为全部历史 Session 无条件创建 Runtime。

最小持久化字段：

```text
session_id
workspace_id
canonical_workspace_path
title
created_at / updated_at / last_opened_at
status
solver model override
effective config revision
message and event history
draft
scroll and UI recovery state
runtime interruption state
```

Session 还包含四个正交维度，不能压成一个互斥枚举：

```text
Lifecycle: Active | Archived | Deleted
Execution: Draft | QueuedForSetup | QueuedForRuntime | Opening
           | Ready | Working | WaitingForUser | Stopping
           | Complete | Interrupted | Offline
Runtime attachment: Detached | Attaching | Attached
Attention flags: Unread | UnresolvedEffect | ConfigPending | RecoveryNeeded
```

### 6.3 User Turn 与 TurnConfig

一条已持久化并被接受的用户消息启动一个 User Turn。Turn 可以包含多次模型调用、工具调用和 Coordinator 判断，直到完成、等待用户、停止或中断。

Turn 开始时从当前 `effective_revision` 解析并锁定 `TurnConfig`：

```text
solver model and effort
coordinator model and effort
agent/system behavior
model deadlines
model-visible tool definitions
```

上述值在同一 Turn 内不变化。Turn 进行时产生的新配置 revision 可以立即保存，但在该 Session 中标记为 `PendingNextTurn`。工具权限收紧和凭据失效属于安全例外：它们可以立即阻止后续尚未开始的操作，但不能伪装成已经改变了正在执行的模型请求。

`Effective` 表示配置解析器已经接受、下一项符合生效边界的新工作将使用的值；它不追溯修改已经创建的 TurnConfig。因而 Working Session 可以同时显示 `TurnConfig revision A` 与 `Effective revision B / Next turn`，这是一种可解释的 pending 状态，不是运行时分裂。

### 6.4 配置作用域与 descriptor

有效优先级：

```text
Current Session override
> Workspace settings
> User settings
> BONE built-in defaults
```

`--model` 在创建 Session 时 materialize 为普通 Session override，并记录 `source=cli`；它不是额外的长期优先级，也不是不可修改的锁。用户可用 `/model` 替换，或用 `/model inherit` 删除 Session override、重新跟随 Workspace/User 默认值。`BONE_MODEL` 若保留，也必须 materialize 为带来源的初始值，不得成为 TUI 无法覆盖的隐藏层。

每个公开设置必须注册一个 descriptor，而不是由设置中心猜测作用域：

```text
key and user-facing label
allowed_scopes
default_scope
inheritance and reset behavior
validation and option provider
apply_boundary
confirmation policy
sensitivity and model visibility
```

作用域矩阵：

| 设置类别 | 合法作用域 | 默认作用域 | 生效/继承说明 |
| --- | --- | --- | --- |
| 当前 Solver、reasoning effort | Session、Workspace、User | 从 `/model` 进入时为 Session；从 Workspace 设置页进入时为 Workspace | Session 可选择“跟随当前工作目录”恢复继承 |
| Coordinator 与 Agent 默认行为 | Workspace、User | Workspace | 不允许 Session 通过普通 `/model` 修改 |
| 工具开关、工具限制、权限策略 | Session、Workspace、User，按具体 capability 声明 | 最窄合法作用域，通常为 Workspace | 安全配置损坏或缺失时 fail closed |
| Appearance、语言、键位 | User | User | 对当前进程下一帧生效 |
| Account、credential、当前 Provider 连接 | User | User | 全局影响需摘要和确认 |
| Workspace root | 无可写作用域 | 不适用 | 当前实例不可修改 |

每个解析后的值至少包含：

```text
value
source
scope
revision
is_inherited
validation_state
apply_boundary
runtime_state
```

### 6.5 Desired、Effective 与 Last Known Good

配置服务维护三个明确概念：

```text
Desired          用户刚确认、希望采用的候选配置
Effective        当前运行时实际使用的配置与 revision
Last Known Good  最近一次持久化且成功应用的完整配置
```

User、每个 Workspace 和每个 Session override 分别维护自己的 scope revision 与 LKG；`effective_revision` 是解析这些层后形成的不可变组合版本。一个 Workspace 的坏候选值不能污染其他 Workspace 的 LKG。

修改流程：

```text
Create candidate Desired
→ schema/domain validation
→ capability and connection preflight
→ prepare replacement runtime resources without touching the old ones
→ atomically persist the candidate revision
→ atomically swap effective runtime handles
→ publish effective_revision
→ replace Last Known Good
```

原则：

- preflight 或资源准备失败时不持久化候选值；
- 持久化后的 runtime swap 必须收敛为不可失败的原子句柄替换；
- 若某类设置无法满足这一点，存储必须显式记录 `desired_revision != effective_revision`，UI 显示“已保存，尚未应用”，并继续使用 Last Known Good；
- 进程在持久化与句柄替换之间崩溃时，下次启动先验证 Desired；验证成功后恢复为 Effective，失败则恢复 Last Known Good 并进入 RepairNeeded；
- 任何情况下都不能仅凭文件写入成功显示“已应用”。

## 7. 核心用户旅程

### 7.1 首次启动

```text
在目录执行 bone
→ 首帧 TUI 出现
→ 后台创建最小内部存储
→ 显示当前工作目录
→ 创建可持久化的 Draft SessionRecord
→ 在 TUI 内连接 ChatGPT
→ 获取真实可用模型
→ 在没有既有 User 默认值时，明确显示并保存 Provider 推荐模型为 User 默认；无法可靠推荐时让用户选择
→ 设置自动保存
→ 惰性附着 Agent Runtime
→ 发出第一条任务
```

要求：

- 不出现配置文件步骤；
- 不要求用户输入模型 ID；
- 首次推荐模型的“所有工作目录”作用范围必须可见，不能静默覆盖既有 User/Workspace 设置；
- TUI 首帧不等待网络；
- 用户可以先写草稿；
- 如果用户确认发送，登录成功后才自动提交；
- 取消登录时完整恢复草稿。

### 7.2 切换当前 Session 模型

```text
/model
→ 打开真实模型选择器
→ 默认范围为“仅当前对话”
→ 用户选择
→ 验证能力
→ 持久化 Session override
→ 显示生效边界
→ 当前 User Turn 保持原 TurnConfig
→ 下一条用户消息启动的 User Turn 使用新模型
```

### 7.3 修改当前 Workspace 默认值

```text
/config
→ Models
→ 修改“当前工作目录”的默认模型
→ 原子保存
→ 继承 Workspace 设置的 Session 在下一个 User Turn 更新
→ 有独立覆盖的 Session 保持不变
→ UI 汇总受影响 Session 数量
```

### 7.4 恢复多个 Session

```text
重新从相同目录执行 bone
→ 解析为同一 Workspace
→ 加载 Workspace Session 索引
→ 恢复上次选中的 Session、历史和草稿
→ 运行中断的任务标记为 Interrupted
→ 按需 hydrate 选中 Session，需要继续工作时才附着 Runtime
→ 用户决定继续、重新提交或保留历史
```

### 7.5 配置损坏

```text
执行 bone
→ TUI 仍然出现
→ 显示“设置需要修复”
→ 保留原始损坏数据
→ 优先使用 Last Known Good；没有 LKG 时权限与工具 fail closed
→ 用户选择自动修复、临时继续或查看诊断
```

### 7.6 One-shot CLI

本 PRD 的 App Shell 和 Overlay 交互仅适用于无消息参数的交互模式，但 one-shot 命令必须复用相同的 Workspace、配置解析、模型目录和 SessionStore 语义：

```text
bone <message>
bone --model <id> <message>
bone --events <path> <message>
```

- `bone <message>` 从当前精确 `cwd` 创建一个持久 SessionRecord，消息 durable 后才启动 Runtime；
- `--model` materialize 为该 Session 的初始 override，不写 Workspace/User 默认值；
- 已有有效认证和模型时直接执行；
- 首次使用且 stdin/stdout 为 TTY 时，提示用户先运行无参数 `bone` 完成 TUI 设置，不降级为手改文件；
- 非 TTY 且缺少认证或模型时，以结构清楚、可执行的错误退出，不尝试设备登录，不打印 secret；
- `--events` 仍是外部观察导出，不替代 durable Session journal；
- one-shot 创建的 Session 默认出现在当前 Workspace 的 Session 列表中，并可在 TUI 中继续；
- CLI/环境来源必须在 `/status` 的技术详情中可解释，TUI 设置仍可覆盖 Session 值。

## 8. 功能需求

优先级定义：P0 是 Beta 入口与 GA 的签字阻塞项；P1 是 GA 目标项，只有记录明确的产品例外才可延期；P2 为后续增强。分阶段实施不能降低 P0 的最终发布门槛。

### 8.1 Bootstrap 与应用生命周期

| ID | 优先级 | 需求 |
| --- | --- | --- |
| BOOT-001 | P0 | 交互模式必须先进入 TUI Shell，再执行强制配置校验、认证、连接和首个 Agent Runtime 附着 |
| BOOT-002 | P0 | 用户配置和数据目录不存在时，自动创建最小合法内部存储并继续启动 |
| BOOT-003 | P0 | 配置、认证、模型或网络故障进入可恢复状态，不在 TUI 前退出 |
| BOOT-004 | P0 | TUI 和 Workspace 可以在 `AgentHost` 尚不存在时运行 |
| BOOT-005 | P0 | Draft SessionRecord 可在未认证、无模型时创建并持久化；仅 Agent Runtime 延迟到依赖就绪后附着 |
| BOOT-006 | P0 | 自动初始化不得向 Workspace 写入 `.bone/` 或修改 `.gitignore` |
| BOOT-007 | P0 | 配置或 SessionStore 不可写时仍先渲染 TUI 错误/恢复页；只有终端本身不可用时才允许在首帧前退出 |
| BOOT-008 | P0 | 无法建立 durable store 时不得接受或执行新消息；可以保留当前内存草稿并允许诊断、重试和安全退出 |

应用级状态：

```text
Bootstrapping
SetupRequired
Authenticating
LoadingModels
Connecting
Ready
Degraded
RepairNeeded
FatalStorageError
```

`FatalStorageError` 是 TUI 内可诊断的阻断页，不是首帧前的 stderr 退出。它必须说明哪些数据尚未持久化，并保留当前进程仍持有的草稿。

### 8.2 配置存储与迁移

| ID | 优先级 | 需求 |
| --- | --- | --- |
| CFG-001 | P0 | 自动创建的用户配置仅包含 schema/version 等稳定字段，不包含假模型或 secret |
| CFG-002 | P0 | 内部配置具有 schema version 和幂等迁移机制 |
| CFG-003 | P0 | 配置服务按 Desired → 预检/准备 → 持久化 → 原子应用 → Effective/LKG 的事务契约处理修改 |
| CFG-004 | P0 | 任一步失败时保留 Last Known Good 为 Effective，并显示 Desired 是否已保存、为何未应用 |
| CFG-005 | P0 | 损坏配置的原始内容必须保留或备份，不得静默覆盖 |
| CFG-006 | P0 | 合法 section 尽可能继续工作，单字段错误不拖垮整份配置 |
| CFG-007 | P0 | token、OAuth code、API key 与普通配置、Session 日志完全隔离 |
| CFG-008 | P0 | 多进程修改使用 revision/CAS；不同字段可安全重放，同字段冲突提供人类化选择，不允许静默 last-write-wins |
| CFG-009 | P1 | 高级用户外部修改配置时，运行中的 BONE 可监听并走相同校验/应用流程 |
| CFG-010 | P0 | 权限、工具、shell、network 等安全设置损坏或不可解析时使用 LKG；无 LKG 时按最小权限 fail closed，自动修复不得扩大权限 |
| CFG-011 | P0 | 磁盘满、权限丢失、文件锁超时和迁移失败均进入可恢复状态，不得把未持久化内容标记为已保存 |

内部建议存储职责：

```text
User config directory
  └── user settings and migrations

User data directory
  ├── workspace registry
  └── per-workspace settings, session index, journals, snapshots, UI state

Secure credential store
  └── access token, refresh token and credential metadata

Workspace directory
  └── no implicit BONE internal data
```

### 8.3 TUI 设置中心

| ID | 优先级 | 需求 |
| --- | --- | --- |
| SET-001 | P0 | `/config` 打开设置中心，不展示原始 JSON |
| SET-002 | P0 | 所有正式公开设置均可在设置中心完成 |
| SET-003 | P0 | 设置支持搜索、键盘导航、说明、枚举选择、开关、受约束输入和“恢复继承” |
| SET-004 | P0 | 每个设置按 descriptor 限制合法 scope，并默认使用当前入口下最窄、最符合语境的 scope；扩大范围必须显式选择 |
| SET-005 | P0 | 每个设置展示当前值、是否继承、影响范围和生效状态 |
| SET-006 | P0 | 一次完整选择即自动保存，不提供总保存按钮 |
| SET-007 | P0 | 设置中心打开时，所有 Session 继续运行，后台完成不抢焦点 |
| SET-008 | P1 | 提供 `/config get/set/reset` 作为高级快捷入口；`/config doctor` 属于 P0 诊断入口 |
| SET-009 | P0 | 危险、权限放宽、账号和全局变更在提交前提供明确影响摘要及必要确认 |
| SET-010 | P1 | 成功修改提供短时撤销能力；撤销也必须走完整配置事务 |
| SET-011 | P0 | 设置中心由同一 descriptor registry 生成公开设置清单；CI/契约测试禁止出现“已公开但无 TUI renderer”或“UI 可改但运行时无消费者”的设置 |

建议分类：

```text
Models
Agent behavior
Tools and permissions
Appearance
Account and connection
Advanced
Diagnostics
```

设置只展示当前注册且真实可用的能力；例如当前没有写工具时，不显示一个可开启但无效的“文件写入”开关。

### 8.4 响应式实时配置

| ID | 优先级 | 需求 |
| --- | --- | --- |
| LIVE-001 | P0 | 所有配置读写经过统一 `ConfigService`，并发布带 revision、diff、scope 与生效边界的事件 |
| LIVE-002 | P0 | TUI、ModelRouter、ConnectionManager、ToolPolicy 和 SessionRuntime 订阅配置事件 |
| LIVE-003 | P0 | Session 不得只持有一份永不更新的完整配置快照 |
| LIVE-004 | P0 | 每个 User Turn 锁定 TurnConfig；同一 Turn 内全部模型调用使用同一 Agent 配置 revision |
| LIVE-005 | P0 | UI 明确区分 Validating、Preparing、Saved、Pending next turn、Applied、Failed/LKG active |
| LIVE-006 | P0 | Workspace/User 设置更新所有仍处于继承状态的 Session，从各自下一个 User Turn 生效，不覆盖 Session override |
| LIVE-007 | P0 | 新连接成功前旧连接保持可用；失败时不破坏旧连接和旧设置 |
| LIVE-008 | P1 | 每个 turn、模型调用和工具调用记录实际使用的配置 revision，默认仅诊断可见 |
| LIVE-009 | P0 | runtime apply acknowledgement 必须携带 effective revision；UI 不得仅根据文件 revision 宣称已应用 |
| LIVE-010 | P0 | TUI 内产生的 Workspace/User 配置变更通过跨进程通知或文件 revision watcher 同步到其他运行实例，并按同一安全边界应用；任意外部手改文件的自动监听仍为 P1 |

安全生效边界：

| 设置 | 生效时间 |
| --- | --- |
| 主题、布局、语言、进度显示 | 当前或下一帧 |
| 当前 Session 模型、推理强度 | 当前 Turn 结束后，下一条用户消息启动的新 Turn |
| Workspace/User 模型默认值 | 继承该值的 Session 下一 User Turn；新 Session 首个 Turn 立即继承 |
| Coordinator 模型和 Agent 行为 | 下一 User Turn，避免一个任务中途混用两套行为 |
| 模型超时 | 下一 User Turn 中的模型 job |
| 模型可见工具集合与 schema | 下一 User Turn，保持 TurnConfig 一致 |
| Host 侧工具执行限制 | 下一次尚未开始的工具调用 |
| 权限收紧 | 当前任务后续工具调度立即受限 |
| 权限放宽 | Host policy 可立即准备；新增模型可见能力从下一 User Turn 使用 |
| 当前 ChatGPT 账号认证与连接 | 新连接准备完成后原子切换；当前 Turn 中已发出的请求不变 |
| Workspace root | 当前实例不可修改 |

### 8.5 模型选择

| ID | 优先级 | 需求 |
| --- | --- | --- |
| MODEL-001 | P0 | `/model` 打开来自当前账号真实能力目录的模型选择器 |
| MODEL-002 | P0 | 默认作用于当前 Session，避免意外影响其他对话 |
| MODEL-003 | P0 | `/model <id>` 修改当前 Session Solver；Idle 时用于下一 Turn，Working 时标记 PendingNextTurn |
| MODEL-004 | P0 | `/model default <id>` 修改当前 Workspace 的默认 Solver |
| MODEL-005 | P0 | `/model global <id>` 修改用户级默认 Solver |
| MODEL-005A | P0 | `/model inherit` 删除当前 Session override，使其重新继承 Workspace/User 默认模型 |
| MODEL-006 | P0 | 普通 `/model` 不修改 Coordinator；Coordinator 位于 Models 的高级设置 |
| MODEL-007 | P0 | 提交前验证账号、Provider、模型和 reasoning effort 兼容性 |
| MODEL-008 | P0 | 错误需区分登录过期、模型不存在、账号不可用、限流、网络和服务故障 |
| MODEL-009 | P0 | 模型目录刷新失败时可使用最近成功缓存，并明确显示缓存时间和验证状态 |
| MODEL-010 | P1 | 高级入口允许手工模型 ID，但不得标记为“已验证可用” |
| MODEL-011 | P0 | P0 仅支持当前 ChatGPT Provider 内选择模型和重连，不展示尚未实现的 Provider/endpoint 切换 |

模型目录的来源和可信度必须可解释：

| 来源 | UI 标记 | 可否直接称为当前账号可用 |
| --- | --- | --- |
| Provider 返回的当前账号 authoritative listing | 已验证 | 可以 |
| 当前会话最近一次成功 listing 缓存 | 已验证于某时间 | 仅在显示缓存时间后可以沿用 |
| BONE 版本化兼容 catalog | 尚未验证 | 不可以，只能作为候选并在提交前做 Provider 能力验证 |
| 用户手工输入 | 未验证 | 不可以，且只在高级入口出现 |

P0 必须提供 `ModelCatalog` 端口。若当前 ChatGPT 服务没有 authoritative listing，则使用版本化 catalog 提供无需记 ID 的候选项，并在应用前通过 Provider 支持的能力检查验证；不得通过一次会计费的普通生成请求伪装成 listing。刷新失败时：

- 已存在成功使用的 Effective/LKG 模型时可继续使用，并标记目录信息是否过期；
- 没有任何已验证模型时进入 `NeedsModel`，说明无法验证的原因并提供重试/重新登录；
- 不得把 catalog 中的候选模型静默标成“当前账号可用”。

### 8.6 Workspace

| ID | 优先级 | 需求 |
| --- | --- | --- |
| WS-001 | P0 | 启动时仅 canonicalize 一次精确 `cwd`，生成统一 `WorkspaceContext` |
| WS-002 | P0 | `WorkspaceContext` 包含稳定 ID、canonical root 与 display root |
| WS-003 | P0 | 所有 Session、工具环境、配置作用域和 SessionStore 使用同一 Workspace 身份 |
| WS-004 | P0 | Workspace 在当前实例中不可改变，不提供 `/cd` |
| WS-005 | P0 | `/workspace` 和 `/status` 显示友好路径、Session 数量、可写状态及可选 Git 元数据 |
| WS-006 | P0 | Workspace 运行中消失或失去权限时进入 Degraded，不静默退回父目录 |
| WS-007 | P0 | 同一账号必须能在多个进程、多个 Workspace 中同时运行；凭据存储不得用连接生命周期长锁阻塞第二个实例 |
| WS-008 | P0 | 两个进程可以并发打开同一 Workspace 的不同 Session；同一个 Session 同时只允许一个可写 Runtime lease |

本地身份：

```text
WorkspaceRegistry[platform namespace + canonical absolute path] = random UUID
```

不能仅用目录 basename、裸路径哈希或 Git remote。平台路径规范遵循 6.1；本地 UUID 不用于遥测。

存储和锁契约：

- 用户设置和 WorkspaceRegistry 使用短时文件锁、revision/CAS 和原子替换，不能在整个 BONE 进程生命周期内持锁；
- Session journal 每个 Session 只有一个 writer，获得可写 Runtime lease 后才能接受新消息；
- 同一 Session 已被其他进程使用时，当前进程可以只读查看、返回该实例，或经明确确认请求接管，不能双写；
- Runtime lease 带单调递增 fencing token；接管必须原子增加 token，使旧 writer 的后续写入被存储层拒绝并促使旧 Runtime 停止，不能只依赖 PID 或超时避免双写；
- 同一 Workspace 的不同 Session 可以并发持有各自 journal lease；
- credential 由用户级 `CredentialBroker` 协调，连接只获取可撤销的 token snapshot/lease；refresh 由 broker 串行化，不能让每个 `AgentHost` 长期独占 credential 文件；
- 如果 CredentialBroker 进程不可用，实现可以使用短时跨进程 refresh lock，但普通 token 读取和既有连接不得被长锁阻塞；
- logout/account switch 由 broker 广播 credential revision，所有进程停止创建新请求并进入明确的重新认证状态；已发出的请求按其连接语义完成或失败，不能静默换账号。

### 8.7 持久化 Session

| ID | 优先级 | 需求 |
| --- | --- | --- |
| SES-001 | P0 | Session 使用跨进程稳定且全局唯一的 ID |
| SES-002 | P0 | Session 创建后永久绑定 `workspace_id` |
| SES-003 | P0 | 已确认消息、最终回复、工具结果、标题、模型覆盖、草稿与恢复所需上下文持续保存 |
| SES-004 | P0 | 再次从相同 Workspace 启动时，只恢复该 Workspace 的 Session 索引 |
| SES-005 | P0 | 恢复上次选中的 Session、草稿和可用历史，不自动打开其他 Workspace 内容 |
| SES-006 | P0 | 崩溃时运行中的任务恢复为 `Interrupted`，不伪装为仍在运行 |
| SES-007 | P0 | 未知或有副作用的工具调用不得在恢复时自动重放 |
| SES-008 | P0 | 单个 Session 损坏不得阻止 Workspace 与其他 Session 打开 |
| SES-009 | P0 | `/new`、`/sessions`、`/resume`、`/rename`、`/archive` 只默认作用于当前 Workspace |
| SES-010 | P0 | 其他 Workspace 的 Session 恢复请求必须拒绝并显示其原目录 |
| SES-011 | P1 | `/delete` 使用可恢复删除或二次确认，不与 archive 混淆 |
| SES-012 | P0 | 用户消息只有在 journal durable append 成功后才显示 Accepted、清空 Composer 并启动 Agent Turn |
| SES-013 | P0 | 启动仅加载 Session index；历史按需 hydrate，Runtime 按需 attach，不为全部历史 Session 建立模型运行时 |
| SES-014 | P0 | 同一 Session 的可写 Runtime lease 互斥；锁冲突时只读打开、定位已有实例或显式接管 |
| SES-015 | P0 | 恢复后提交的新消息必须获得已完成历史的模型可见上下文，而不仅是恢复 UI timeline |
| SES-016 | P0 | Runtime 数量受资源预算约束；Working Runtime 不被静默驱逐，Idle/Complete Session 只有在 durable 后才可 detach；预算耗尽时新 Turn 明确进入 QueuedForRuntime，不能静默停止其他 Session |

恢复是冷恢复：历史和上下文可以继续，进程内 Future、网络流和正在运行的外部命令不会跨进程继续。Session rail 默认展示当前 Workspace 的 Active Session 元数据；选择 Session 时 hydrate，提交新消息时 attach Runtime。`/sessions` 打开当前 Workspace 的 Session picker，`/resume` 是选择并 hydrate/attach 一个持久 Session，不代表恢复旧 Future。Archived Session 默认不出现在 rail，可在 picker 中筛选。

#### 8.7.1 Durable acknowledgement 与 RPO

“已确认用户消息”具有严格定义：

```text
Composer text
→ append complete UserMessage record to per-session journal
→ flush according to durable store contract
→ return durable acknowledgement
→ clear Composer and mark Accepted
→ start User Turn
```

规则：

- durable acknowledgement 至少发生在 journal/database transaction commit 并完成平台可用的同步落盘屏障之后；RPO 0 的故障域覆盖正常退出、进程崩溃和强制终止，不承诺存储硬件损毁；
- journal 写入失败、磁盘满或 writer lease 丢失时，不清空 Composer、不启动 Agent；
- clean shutdown 下已展示的完整草稿、已确认消息和完成事件 RPO 为 0；
- abnormal crash 下已确认消息 RPO 为 0；
- 草稿按输入 debounce、焦点切换、Overlay 打开、Session 切换和退出保存，异常崩溃允许最多 1 秒的最新输入 RPO，并在恢复时说明是否只恢复到最后草稿 revision；
- 流式回复可以先渲染内存中的增量，但 `Complete` 只能在最终回复和完成边界 durable 后显示；异常崩溃时未完成流恢复为 `Interrupted partial output`，不能伪装成完整回答；
- 工具开始、结果和 external-effect 状态使用可校验记录；未知结果恢复为 `UnresolvedEffect` attention flag。

#### 8.7.2 Session continuation contract

SessionStore 必须保存或可确定性重建以下 model-visible context：

```text
completed user/assistant turns
model-visible tool calls and conclusive results
approved compaction summaries and their source range
interrupted-turn boundary
agent protocol/system-prompt version
tool-schema version
historical model/config revision metadata
```

恢复语义：

- 已完成 Turn 可进入后续模型上下文；
- 未完成 Turn 以明确的 Interrupted boundary 结束，只保留可证明完成的消息和工具结果；
- 不重新执行历史工具或副作用；
- 新 Turn 使用当前 Effective 配置并创建新的 TurnConfig，历史仍保留其原模型/revision 元数据；
- 历史超出上下文窗口时使用已持久化且可追溯的 compaction summary；
- 协议或 system prompt 版本不兼容时，Session 可以只读打开，但在完成迁移或用户确认新的 continuation boundary 前不能伪装成可无损继续；
- 可视 timeline 不是恢复上下文的唯一数据源。

### 8.8 Slash Command 系统

| ID | 优先级 | 需求 |
| --- | --- | --- |
| CMD-001 | P0 | 所有命令由统一 typed registry 定义，不能继续散落为字符串判断 |
| CMD-002 | P0 | 输入 `/` 即打开可搜索、可补全的命令面板 |
| CMD-003 | P0 | 内置控制命令在本地执行，不作为用户 Prompt 进入模型上下文 |
| CMD-004 | P0 | 未知 `/xxx` 不发送给模型，显示拼写建议 |
| CMD-005 | P0 | 粘贴多行文本不自动执行 slash command |
| CMD-006 | P0 | 每个命令声明作用域、可用状态与 busy 时执行策略 |
| CMD-007 | P0 | 命令结果作为本地系统反馈显示，但不污染模型上下文 |
| CMD-008 | P0 | 用户需要发送 `/` 开头的普通文本时，可用 `//` 转义或选择“作为消息发送” |
| CMD-009 | P0 | slash command 只有在单逻辑行、第一处非空字符为 `/` 且用户确认提交时才执行；粘贴本身永不执行命令 |
| CMD-010 | P0 | `/exit` 在有 Working Session 时显示影响摘要；退出流程停止接受新消息、durable 当前状态、请求停止 Runtime，并将未完成工作记录为 Interrupted/UnresolvedEffect |

每个命令至少声明：

```text
name
aliases
title and description
argument schema
autocomplete provider
availability predicate
scope
execution policy
model visibility
```

P0 命令：

```text
/help
/status
/config
/config doctor
/model
/model inherit
/login
/logout
/workspace
/new
/sessions
/resume
/rename
/archive
/stop
/exit
```

Busy 时：

- `/help`、`/status`、`/config`、`/workspace` 立即执行；
- `/model` 立即保存；Idle 时用于下一 Turn，Working 时明确标记 PendingNextTurn；
- `/stop` 立即请求停止当前 Session；
- `/new` 立即创建独立 Session；
- 不允许的命令仍显示，但明确说明不可用原因；
- 命令不能无反馈地排在普通 Prompt 后。

解析和转义规则：

- 单行 Composer 中第一处非空字符为 `/` 时进入命令候选模式；
- 只有匹配已注册命令且用户按 Enter 确认后才执行；
- `//foo` 作为普通消息 `/foo` 发送；
- 未知命令不自动发送，提供拼写建议和“作为消息发送”动作；
- 多行 paste 默认保持普通消息语义，即使第一行以 `/` 开头；
- 单行 paste 可以显示命令候选，但 paste 事件本身不提交；
- `/exit`、`/logout`、接管 Session 等有广泛影响的命令仍需按各自 confirmation policy 执行。

### 8.9 Authentication 与账号切换

| ID | 优先级 | 需求 |
| --- | --- | --- |
| AUTH-001 | P0 | `/login`、首次登录、重新认证和 device flow 全程在 TUI Overlay 内完成，取消、过期和失败均返回可操作状态且不丢草稿 |
| AUTH-002 | P0 | Account/credential 是 User scope；登录或切换账号不得被伪装成当前 Session 私有设置 |
| AUTH-003 | P0 | `/logout` 显示所有受影响的本地 BONE 实例/Session 摘要并确认，由 CredentialBroker 删除或吊销本地 credential、发布新 credential revision |
| AUTH-004 | P0 | logout/account switch 后停止创建新模型请求；已发出的请求使用原 credential 完成或明确失败，不能在一次请求中静默换账号 |
| AUTH-005 | P0 | SessionRecord、历史和草稿保持本地可见；新账号首次向旧 Session 发起 Turn 前再次确认数据将发送给新账号 |
| AUTH-006 | P0 | device code、token 和 authorization header 不进入持久 transcript、普通日志、诊断导出或模型上下文 |

`/logout` 不删除 Session。若当前有 Working Session，确认页必须说明：新模型请求将停止，当前请求可能完成或失败，草稿与历史仍保留。用户取消确认时不得改变 credential 或连接状态。

### 8.10 状态、诊断与恢复

| ID | 优先级 | 需求 |
| --- | --- | --- |
| DIAG-001 | P0 | `/status` 展示 Workspace、当前 Session、当前 TurnConfig、下一 Turn Effective 值、配置来源/待生效变更、认证与连接状态 |
| DIAG-002 | P0 | `/config doctor` 检查 Desired/Effective/LKG、WorkspaceRegistry、SessionStore/lease、CredentialBroker、目录权限和模型目录/连接 |
| DIAG-003 | P0 | 默认错误回答：发生了什么、用户数据是否安全、现在可以做什么 |
| DIAG-004 | P0 | 技术路径、内部 key、revision 与错误链仅在“技术详情”中展示 |
| DIAG-005 | P1 | 可导出脱敏诊断，且不包含 Prompt、回复、token、文件内容或原始敏感路径 |

## 9. 前端信息架构

```text
BONE App Shell
├── Workspace Context
├── Session Navigation
├── Workbench
│   ├── Conversation timeline
│   ├── Live activity
│   ├── Composer
│   └── Status line
├── Overlay
│   ├── Command Palette
│   ├── Settings Center
│   ├── Model Picker
│   ├── Session Picker
│   ├── Authentication
│   ├── Diagnostics
│   └── Confirmation / Recovery
└── Non-blocking Feedback
    ├── Toast
    ├── Inline field state
    ├── Local timeline event
    └── Persistent warning banner
```

应用生命周期、Session 生命周期和界面 Overlay 必须是三个正交状态，不能把所有状态塞进同一个 Session 枚举。

同一时刻最多一个阻塞式 Overlay。Overlay 捕获按键的优先级高于 Composer；关闭 Overlay 的 `Esc` 不能同时停止 Agent。

状态展示必须区分配置意图与实际运行值：设置字段可以显示 Desired 候选和验证进度；会话状态栏始终显示 Effective/TurnConfig；待下一 Turn 生效时同时显示 `Current: A` 与 `Next turn: B`。失败横幅必须说明 Last Known Good 仍在使用，不能只给一个通用错误 toast。

## 10. 非功能需求

### 10.1 性能

- 本地环境从执行 `bone` 到首帧 TUI，P95 ≤ 1 秒；
- 首帧不得等待登录、模型目录或网络；
- 本地 UI 设置变更的视觉反馈 ≤ 100 ms；
- 本地校验与持久化 P95 ≤ 500 ms；
- 网络设置必须在 100 ms 内进入“正在验证/连接”，不要求网络本身在 500 ms 完成；
- 配置事件与 Agent 高频事件需要合并，避免终端闪烁。

### 10.2 可靠性

- 干净退出后的 Session 恢复成功率目标 ≥ 99.99%；
- 已确认用户消息在正常退出和可恢复异常崩溃后的丢失率为 0；
- 异常崩溃下草稿最新输入的 RPO ≤ 1 秒，正常退出 RPO 为 0；
- 最终回复与完成边界未 durable 前不得显示 Session Complete；
- UI 宣称已应用但 Runtime 仍使用旧 revision 的已知事件为 0；
- 未知副作用自动重放次数为 0；
- 跨 Workspace 静默重新绑定次数为 0；
- 同一个 Session 同时出现两个 journal writer 的次数为 0；
- 同一账号打开第二个 Workspace 因长生命周期 credential 文件锁失败的次数为 0。

### 10.3 可访问性与终端兼容性

- 所有主流程可纯键盘完成；
- 颜色不是唯一状态信号，必须配符号与文字；
- 支持 `NO_COLOR` 或高对比模式；
- 兼容 40、80、120 列代表性终端；Windows Terminal 的 P0 认证指 WSL2 宿主，Native Windows 遵循平台矩阵；
- 小于最小尺寸时提示窗口过小，但不丢草稿、不停止后台任务；
- 所有状态符号通过 CJK/Windows Terminal/iTerm/GNOME Terminal 宽度测试；
- 焦点始终唯一可见，后台任务完成不得抢焦点。

### 10.4 安全与隐私

- OAuth token、refresh token、API key、authorization header 与 device code 不进入普通配置、Session transcript 或诊断导出；
- Device code 只在认证 Overlay 生命周期内显示；
- Workspace 数据默认存储在用户数据目录，不写入仓库；
- 用户设置、WorkspaceRegistry 和 SessionStore 默认使用当前用户私有权限；Unix 目录/文件至少遵循 `0700/0600` 等价语义，其他平台使用相应 ACL；拒绝不安全的 symlink、hardlink 或所有者异常；
- 本地控制命令和设置数据不进入模型上下文；
- 诊断复制自动脱敏；
- 配置损坏时安全相关设置 fail closed，且自动恢复不得扩大权限；
- 模型默认不能自主修改用户设置。未来若开放配置工具，必须逐次显示 diff、scope 和影响并获得用户批准，且必须走同一 ConfigService 事务；
- 引入写工具前必须实现 Workspace 写协调、文件 revision 检查或可选 worktree 隔离。

### 10.5 存储、容量与锁

- WorkspaceRegistry、配置和 Session index 使用短时锁与原子更新，不持有进程生命周期文件锁；
- Session journal 使用有边界、可校验的记录格式，尾部部分写入可安全截断，单 Session 损坏不扩散；
- Session snapshot/compaction 不得在成功替换前删除其来源 journal；
- 磁盘不足时提前显示持久警告；一旦无法 durable append，不接受新消息、不清空 Composer；
- Archive 只改变可见性，不等同于释放空间；
- P1 设置页提供存储占用、导出、可恢复删除和 compact 管理；P0 不得自动删除用户 Session；
- stale Runtime lease 必须通过持有者身份和 heartbeat/进程存活校验判断，不能仅凭超时偷锁；显式接管前显示可能的运行中任务和副作用风险。

## 11. 成功指标

### 11.1 北极星指标

**无文件干预完成率**：首次启动用户中，不打开配置文件、不离开 TUI 修复设置，就完成首次有效 Agent 请求的比例。

- Beta 目标：≥ 90%；
- 稳定版目标：≥ 95%。

### 11.2 关键指标

| 指标 | 目标 |
| --- | --- |
| 无配置启动进入 TUI 成功率 | ≥ 99.9% |
| 因配置问题在 TUI 前退出 | < 0.1% |
| `/config` 内完成常用设置成功率 | ≥ 95% |
| 正式公开设置无需 restart/reload 的比例 | 100% |
| 未知 slash command 被误发给模型 | 0 |
| 模型不可用错误给出可执行恢复动作 | 100% |
| 不同 Workspace Session 混入 | 0 |
| 已确认消息丢失 | 0 |
| Effective revision 误报 | 0 |
| 同一 Session 双 writer | 0 |
| 第二 Workspace 被 credential 长锁阻断 | 0 |

### 11.3 建议脱敏事件

```text
tui_first_frame
bootstrap_state_entered
setup_started / completed / failed
settings_opened
setting_changed(category, scope, result, latency)
slash_command_invoked(command_name, result)
workspace_opened(existing/new)
session_created / resumed / interrupted
config_recovery_started / completed
connection_swap_started / completed / failed
```

不得采集 Prompt、回复正文、文件内容、token、原始绝对路径或 Session transcript。

### 11.4 指标字典与隐私前提

| 指标 | 起点 | 成功终点 | 排除/失败规则 |
| --- | --- | --- | --- |
| 无文件干预完成率 | 新安装首次 `tui_first_frame` | 首个 User Turn durable 完成 | 用户主动取消单独记录；配置/认证/模型错误计入未完成 |
| 无配置启动成功率 | 检测到用户配置不存在 | 首帧 App Shell 可交互 | 非 TTY、终端初始化失败不计入配置失败 |
| 设置任务成功率 | 用户打开某设置项 | 对应 `effective_revision` 被运行时确认 | 用户取消单列；验证、持久化或应用失败计为失败 |
| Session 恢复成功率 | 已存在 Session 的 Workspace 启动 | index、上次选中 Session 和可用历史恢复 | 用户主动新建不算失败；损坏隔离单独记录 |
| 首次有效 Agent 请求 | 用户消息得到 durable acknowledgement | Turn 到达 Complete 或 WaitingForUser 且边界 durable | Provider 故障单独分层，但仍保留端到端失败指标 |

产品指标只能在适用的隐私政策和用户遥测选择下采集。没有遥测授权时，通过本地集成测试、故障注入和用户研究验证，不得为了计算指标扩大采集范围。Workspace 相关埋点只使用每安装随机 salt 派生的匿名标识，不能上传 canonical path、本地 workspace UUID 或可跨安装关联的稳定 ID。

## 12. 验收标准

### AC-01：零配置首次启动

```gherkin
Given 用户配置目录和配置文件不存在
When 用户在 TTY 中执行 bone
Then 全屏 TUI 成功出现
And 用户无需创建或编辑任何文件
And /help、/config、/status、/exit 可用
And 内部存储在后台安全创建
```

### AC-02：缺少旧必填配置

```gherkin
Given 配置文件存在但没有 agent.system
When 用户执行 bone
Then 不得在进入 TUI 前退出
And TUI 使用安全默认值或引导模型选择
And 不显示要求手工修改 JSON 的错误
```

### AC-03：配置损坏

```gherkin
Given 配置文件含无效 JSON
When 用户执行 bone
Then TUI 仍然出现
And 原始内容被保留或备份
And 用户可以在 TUI 内自动修复、临时继续或查看诊断
```

### AC-04：实时显示设置

```gherkin
Given 用户在 /config 修改主题或进度显示
When 用户确认选择
Then 当前 TUI 在下一帧使用新值
And 设置被自动保存
And 重启后仍保持
```

### AC-05：运行中模型切换

```gherkin
Given 当前 Session 正在执行模型请求 A
When 用户选择模型 B
Then 当前 User Turn 内所有后续模型调用仍使用原 TurnConfig 和模型 A
And UI 显示 B 将从下一条用户消息生效
And 下一条用户消息启动的新 User Turn 使用 B
And 不重启或重建 Session
```

### AC-06：作用域继承

```gherkin
Given Session 1 继承 Workspace 模型
And Session 2 有独立模型覆盖
When 用户修改 Workspace 模型
Then Session 1 的当前 Turn 不变并在下一 User Turn 使用新模型
And Session 2 保持原模型
And UI 明确汇总两者
When 用户在 Session 2 选择“跟随当前工作目录”或执行 /model inherit
Then Session 2 删除自身 override 并在下一 User Turn 继承 Workspace 模型
```

### AC-07：连接热更新

```gherkin
Given 当前连接可用
When 用户修改需要重连的设置
Then TUI 立即显示正在连接
And 旧连接在新连接成功前保持可用
And 新连接成功后原子切换
And 失败时旧连接与旧设置继续可用
```

### AC-08：精确 Workspace

```gherkin
Given /repo 和 /repo/packages/a 都存在
When 用户分别从两个目录启动 bone
Then 系统识别为两个不同 Workspace
And 不因它们属于同一 Git 仓库而合并
```

### AC-09：Session 持久化

```gherkin
Given 用户在 Workspace W 创建多个 Session
And Session 包含历史、草稿和模型覆盖
When 用户退出并再次从 W 启动
Then 这些 Session 重新出现
And 历史、草稿和覆盖值保持
And 上次选中的 Session 被恢复
```

### AC-10：跨 Workspace 隔离

```gherkin
Given Session S 属于 Workspace A
When 用户从 Workspace B 尝试恢复 S
Then 系统拒绝
And 告知 S 所属目录
And 不修改 S 的 workspace_id
And 不改变当前 Workspace
```

### AC-11：本地命令隔离

```gherkin
Given 用户执行 /model、/config 或 /status
When 命令完成
Then 模型输入中不存在命令文本和设置数据
And TUI 可以显示本地系统反馈
```

### AC-12：未知命令

```gherkin
Given 用户输入 /modle
When 用户提交
Then 系统不发送给模型
And 提示可能的 /model
```

### AC-13：崩溃恢复

```gherkin
Given 用户消息已经得到本地确认
And BONE 在任务运行时异常退出
When 用户再次从同一 Workspace 启动
Then 已确认消息仍存在
And Session 的 Execution 状态为 Interrupted
And 未知副作用不会自动重跑
```

### AC-14：普通用户完整闭环

测试用户必须能够完成：

```text
启动
→ 登录
→ 自动或手动选择模型
→ 发送任务
→ 修改模型
→ 新建 Session
→ 退出
→ 恢复 Session
```

全过程不得要求：

- 打开配置文件；
- 输入配置路径；
- 复制示例 JSON；
- 查询模型 ID 文档；
- 手动重启或 reload 以应用设置。

### AC-15：未登录时的逻辑 Session 与草稿

```gherkin
Given 用户尚未登录且没有可用模型
When 用户启动 bone 并在 Composer 输入内容
Then 系统已经创建绑定当前 Workspace 的持久 SessionRecord
And 尚未创建 Agent Runtime
And 用户取消、超时或登录失败后草稿仍存在
And 重新启动后可恢复最后 durable 的草稿 revision
When 用户在设置完成前确认发送
Then 消息先 durable 为 QueuedForSetup 且只排队一次
And 登录与模型就绪后才自动启动 Turn
And 用户取消设置时排队消息恢复为可编辑草稿，不会在未来静默发送
```

### AC-16：Desired、Effective 与 LKG 事务

```gherkin
Given 当前 Effective/LKG 配置为 revision A 且连接可用
When 用户提交需要新连接的 Desired revision B
Then 系统先准备 B 的候选连接且 A 保持可用
When B 的预检或准备失败
Then B 不得被显示为已保存或已应用
And Effective 与 LKG 均保持 A
When B 已持久化但进程在 runtime swap 前异常退出
Then 下次启动验证 B 后应用它，或恢复 A 并进入 RepairNeeded
And 不得出现无法解释的文件值与运行时值分裂
```

### AC-17：部分配置损坏与 fail-closed

```gherkin
Given 配置 JSON 语法有效
And Appearance section 合法
And Tools/Permissions section 含无效字段
When 用户启动 bone
Then TUI 使用合法的 Appearance 设置
And Tools/Permissions 使用 Last Known Good
And 没有 Last Known Good 时按最小权限禁用相关能力
And 自动修复不得扩大权限
And 用户可以在 TUI 中修复该 section
```

### AC-18：持久化失败不产生假确认

```gherkin
Given 当前 SessionStore 磁盘已满、不可写或 writer lease 已丢失
When 用户提交 Composer 消息
Then Composer 文本保持完整
And 消息不显示 Accepted
And Agent Turn 不启动
And TUI 说明未保存及恢复动作
```

### AC-19：异常退出的 durable acknowledgement

```gherkin
Given 用户消息已收到 durable acknowledgement
And Agent 最终回复尚未 durable
When 进程异常退出并从同一 Workspace 恢复
Then 用户消息仍存在
And Session 显示 Interrupted
And 未完成回复可标为 partial 但不能显示 Complete
And 未知工具副作用不自动重放
```

### AC-20：恢复后的模型上下文连续性

```gherkin
Given Session 已完成一轮且用户提供了一个只存在于该对话中的事实
When 用户退出、重新打开同一 Workspace 并 resume 该 Session
And 用户在新 Turn 询问该事实
Then 新模型输入包含可确定性恢复的已完成历史或其可追溯 compaction summary
And 不重新执行历史工具
And 新 Turn 使用当前 Effective 配置
```

### AC-21：索引、hydrate 与 Runtime 惰性附着

```gherkin
Given 当前 Workspace 存在大量历史 Session
When 用户启动 bone
Then 首先只加载 Session index 和上次选中 Session 所需数据
And 不为所有历史 Session 创建 Agent Runtime
When 用户从 /sessions 选择某个 Session
Then 系统按需 hydrate
When 用户提交新消息
Then 系统按需 attach Runtime 后启动 Turn
Given Runtime 预算已被 Working Session 占满
Then 新 SessionRecord 和草稿仍可创建
And 新 Turn 明确显示 QueuedForRuntime
And 不静默停止或驱逐正在工作的 Session
```

### AC-22：多 Workspace 共用账号并发

```gherkin
Given 用户在进程 A 的 Workspace A 已登录并保持连接
When 用户在进程 B 的 Workspace B 启动 bone
Then B 可以读取有效认证并建立连接
And 不因 A 持有长生命周期 credential 文件锁而失败
And refresh 操作由 CredentialBroker 或短时跨进程锁串行化
```

### AC-23：同一 Session writer lease

```gherkin
Given 进程 A 已对 Session S 持有有效可写 Runtime lease
When 进程 B 打开同一 Workspace 和 Session S
Then B 不得获得第二个 journal writer
And B 可以只读查看、定位 A 或请求显式接管
And 未确认接管前不能接受新消息
When 用户完成允许的接管
Then lease fencing token 原子递增
And A 的旧 token 后续写入被拒绝且旧 Runtime 停止
```

### AC-24：slash command 转义和粘贴

```gherkin
Given 用户输入 //tmp/example
When 用户提交
Then 模型收到普通消息 /tmp/example
Given 用户粘贴以 /stop 开头的多行文本
When 粘贴事件发生
Then 不执行 /stop
And 文本保持普通多行消息语义
Given 用户输入未知命令 /modle
When 用户提交
Then 不自动发送给模型
And 用户可以选择建议的 /model 或“作为消息发送”
```

### AC-25：Busy 与 Overlay 键盘优先级

```gherkin
Given 当前 Session 正在 Working 且 Settings Overlay 已打开
When 用户按 Esc
Then 只关闭 Overlay
And 不向 Session 发送 stop
When 用户在 Working 时执行 /status、/config 或 /new
Then 命令立即执行且不等待当前模型完成
When 用户执行 /model
Then 新值保存为 Pending next turn
```

### AC-26：模型目录可信度

```gherkin
Given Provider 无 authoritative listing
When BONE 使用版本化 catalog 展示候选模型
Then UI 将候选标记为尚未验证
And 应用前使用 Provider 支持的非普通生成能力检查
And 不要求用户查询或手输模型 ID
Given 用户已有成功使用的 LKG 模型且目录刷新失败
Then 用户可继续使用该模型并看到目录缓存时间
```

### AC-27：平台 Workspace 等价与隔离

```gherkin
Given 在任一已声明支持的平台中，两个启动路径通过 symlink 或该平台等价表示指向同一真实目录
When 分别解析 Workspace
Then 在同一平台命名空间中得到相同 Workspace UUID
Given /repo 与 /repo/packages/a 是不同真实目录
Then 它们得到不同 Workspace UUID
And 遥测中不出现 canonical path、本地 UUID 或无盐路径 hash
And Native Windows 获得支持时同一用例扩展覆盖 junction、UNC、盘符和大小写语义
```

### AC-28：Workspace 运行中不可访问

```gherkin
Given BONE 已在 Workspace W 启动
When W 被删除、卸载或失去访问权限
Then TUI 保持可操作并进入 Degraded
And 新的 Workspace 工具操作被阻止
And 系统不退回父目录或改变 workspace_id
And 草稿、Session 历史和诊断仍可访问
```

### AC-29：账号登出与切换

```gherkin
Given 同一账号有多个运行中的 BONE 实例和 Session
When 用户执行 /logout
Then TUI 显示影响摘要并要求确认
When 用户取消
Then credential 和连接均不变化
When 用户确认
Then CredentialBroker 更新 credential revision 并阻止所有实例创建新模型请求
And Session、历史和草稿不被删除
And 使用新账号向旧 Session 发起首个 Turn 前再次确认数据接收方变化
```

### AC-30：One-shot CLI 兼容

```gherkin
Given 当前 Workspace 已在 TUI 完成认证和模型设置
When 用户执行 bone --model M "task"
Then 创建一个绑定当前 Workspace 的持久 Session
And M 作为该 Session 的普通 override
And 消息 durable 后才开始执行
And 该 Session 后续可在 TUI 恢复
Given 非 TTY 首次运行且没有认证
Then 命令提供运行交互式 bone 的明确指引
And 不要求手改配置且不打印 device code 或 secret
```

### AC-31：Secret 与 Agent 配置权限隔离

```gherkin
Given 用户完成登录、运行 /config doctor 并导出或复制诊断
Then token、refresh token、device code 和 authorization header 不存在于 transcript、普通日志、模型输入或诊断内容
And 用户设置、WorkspaceRegistry 与 SessionStore 具有当前平台的用户私有权限
And 不安全的链接或所有者异常不会被静默接受
Given Agent 尝试自主修改设置
Then 默认拒绝
And 未来启用配置工具时必须先向用户显示 diff、scope 和影响并获得批准
```

### AC-32：单 Session 损坏隔离

```gherkin
Given Workspace 有 Session A、B、C
And B 的 journal 有损坏尾记录
When 用户启动 bone
Then Workspace、A 和 C 正常打开
And B 截断到最后可校验边界或进入只读 RecoveryNeeded
And 不覆盖 B 的原始损坏数据
```

### AC-33：启动时数据目录不可写

```gherkin
Given 用户配置或数据目录在启动前不可写
When 用户在 TTY 中执行 bone
Then 首帧 TUI 仍然出现并进入 FatalStorageError 恢复页
And /help、/status、/config doctor、重试和 /exit 可用
And 系统不接受或执行无法 durable 的新消息
And 不要求用户通过手工编辑 JSON 修复
```

### AC-34：配置迁移失败

```gherkin
Given 用户存在旧 schema 版本配置及 Last Known Good
When 自动迁移在校验或持久化阶段失败
Then 原配置和迁移前备份均保留
And Effective 继续使用 Last Known Good 或进入 fail-closed 恢复态
And TUI 展示可执行的重试、恢复和诊断动作
And 不生成一份看似成功的新空配置
```

### AC-35：多进程配置冲突

```gherkin
Given 进程 A 与 B 读取同一配置 revision
When A 修改 Appearance 且 B 修改 Workspace 模型
Then 配置服务重新读取并安全保留两个不冲突的字段修改
Given A 与 B 修改同一字段为不同值
Then 后提交者看到人类化冲突选择
And 系统不静默 last-write-wins
And 当前 Effective/LKG 始终可解释
```

### AC-36：公开设置注册完整性

```gherkin
Given 一个设置 descriptor 被标记为公开
When 运行配置契约测试
Then 它具有合法 scope、默认值、校验、TUI renderer、应用消费者和生效边界
And 设置中心可以发现并修改它
Given 一个能力尚未注册或 runtime 尚无消费者
Then 设置中心不展示一个虚假的可用设置
```

### AC-37：安全退出

```gherkin
Given 当前 Workspace 有一个或多个 Working Session
When 用户执行 /exit
Then TUI 显示将被停止的 Session 和未决副作用摘要
When 用户确认退出
Then 系统停止接受新消息并 durable 所有已确认状态
And 请求停止所有 Runtime
And 超过安全等待边界的任务记录为 Interrupted 或 UnresolvedEffect
And 终端状态被恢复
And 下次启动可以解释每个 Session 的最终状态
```

### AC-38：跨进程实时设置

```gherkin
Given 进程 A 与 B 使用同一 User 设置
And B 的 Session 仍继承该设置
When A 在 TUI 修改 User scope 设置并得到 Effective acknowledgement
Then B 收到新 revision 且无需 restart/reload
And 展示设置按下一帧更新
And Agent 设置按 B 的下一 User Turn 更新
And B 中存在 Session override 的值不被覆盖
```

## 13. 风险与缓解

| 风险 | 影响 | 缓解 |
| --- | --- | --- |
| “实时”只刷新 UI | 用户看到新值但 Runtime 用旧值 | ConfigService revision、运行时确认与端到端测试 |
| 下一模型调用被误当成下一用户轮次 | 一个任务中途混用模型和 Agent 行为 | User Turn/TurnConfig 锁定与 PendingNextTurn 状态 |
| Desired 已持久化但 runtime apply 失败 | 文件与实际行为分裂 | 候选资源预热、原子句柄替换、Effective/LKG 与崩溃恢复契约 |
| 当前 ModelAdapter 固定模型 | `/model` 无法热切换 | 引入每 Session 的动态 ModelRouter |
| 配置损坏时自动覆盖 | 用户设置丢失 | 原始内容备份、容错打开、显式修复 |
| 安全配置损坏后宽松回退 | 意外扩大工具或网络权限 | LKG 优先、无 LKG 时 fail closed、权限变更摘要 |
| 多实例配置冲突或不同步 | 设置被覆盖，其他 TUI 继续用旧值 | 短时锁、CAS、字段级 patch、跨进程 revision 通知、冲突 UI |
| 模型目录依赖网络或缺 listing API | 首次设置卡住或把候选误报可用 | ModelCatalog 来源标签、LKG/cache、非生成式能力验证 |
| Session 日志损坏或磁盘满 | 历史丢失或假确认 | durable ack、可校验 journal、快照、单 Session 隔离、写失败保留 Composer |
| 恢复只有可视历史、没有模型上下文 | 用户以为能继续但模型失忆 | Session continuation contract 与恢复后上下文测试 |
| 同一 Session 被两个进程写入 | journal 分叉或任务重复 | per-session Runtime lease、只读打开、显式安全接管 |
| 多 Session 同目录写文件 | 文件相互覆盖 | Workspace 单写者、revision 检查或 worktree |
| 设置中心过于复杂 | 用户仍需查文档 | 推荐值、基础/高级分层、搜索和说明 |
| 作用范围误操作 | 意外影响其他 Session/项目 | 每项 descriptor 的最窄合法 scope、inherit、全局影响摘要 |
| 多 Workspace 进程争用 credential | 第二个实例不能连接 | CredentialBroker 或短时 refresh lock，禁止连接生命周期长锁 |
| 登出或切换账号静默影响旧 Session | 数据发给错误账号或后台任务异常 | 全局影响确认、credential revision 广播、新账号首个 Turn 再确认 |

## 14. 分阶段交付

阶段用于降低实现风险，不代表允许发布一个仍需手工配置的正式版本。

### Phase A：架构契约

- 固定 Workspace 规范化和身份规则；
- 定义配置 descriptor、作用域、Desired/Effective/LKG 和 TurnConfig；
- 定义 SessionRecord、durable acknowledgement、continuation schema 与 Runtime attachment；
- 定义 App、Session 多维状态与 Overlay 状态机；
- 定义用户设置、WorkspaceRegistry、Session journal 和 credential 的锁/代理契约；
- 定义 typed command registry；
- 建立契约测试。

退出条件：核心作用域和恢复语义不存在悬而未决的产品歧义。

### Phase B：TUI-first 与实时设置

- TUI Shell 先于配置、认证和 AgentHost；
- 零配置自动初始化；
- 配置损坏进入修复态；
- TUI 内登录；
- `/config` 设置中心；
- `/model` 真实模型选择器；
- ModelCatalog 来源/验证/cache；
- ConfigService、ModelRouter、ConnectionManager 与 CredentialBroker；
- 多进程配置 CAS/通知、LKG/fail-closed 和账号并发；
- `/model inherit`、账号切换确认与 slash 转义；
- `/help`、`/status`、`/workspace`、`/config doctor`。

退出条件：用户无需配置文件即可完成首次任务；运行中模型切换明确 PendingNextTurn；第二个 Workspace 不被 credential 长锁阻断。

### Phase C：Workspace 持久化多 Session

- 稳定 Workspace ID 与 Session ID；
- Session journal、snapshot、索引与 per-session writer lease；
- durable acknowledgement、草稿 RPO 与磁盘失败保护；
- model-visible continuation、compaction 与协议版本边界；
- 草稿、标题、状态和模型覆盖恢复；
- `/new`、`/sessions`、`/resume`、`/rename`、`/archive`；
- index-only 启动、按需 hydrate 与 Runtime attach/detach；
- Interrupted、partial output 和 unknown-effect 冷恢复；
- 跨 Workspace 严格隔离。

退出条件：从相同目录重启后能恢复多个 Session 的 UI 与模型上下文；已确认消息不丢失；不能出现双 writer 或跨 Workspace 混入。

### Phase D：Beta/GA 产品硬化

- 对多进程冲突、commit/swap 崩溃、磁盘满、Session 损坏和 migration 回滚做故障注入；
- 登录与连接异常矩阵；
- 长历史、终端尺寸和可访问性测试；
- 脱敏诊断；
- one-shot CLI 兼容与模型目录降级矩阵；
- 引入写工具前的 Workspace 写协调。

退出条件：全部 P0 验收标准通过，关键可靠性与隐私指标达到目标。

## 15. 当前实现差距

| 产品能力 | 当前状态 | 现有基础与缺口 |
| --- | --- | --- |
| TUI 先于配置启动 | 不满足 | [`main.rs`](../../crates/bone-app/src/main.rs) 在 TUI 前读取配置并连接 |
| 无配置可用 | 部分满足 | `bone-config` 缺文件按空对象读取，但 `agent.system` 仍必填 |
| 原子配置写入 | 已有基础 | ConfigManager 已有锁、CAS revision 与原子替换 |
| 配置事务与变更事件 | 不满足 | 当前没有 Desired/Effective/LKG、watch/channel 或 runtime apply acknowledgement |
| 当前 Session 响应配置 | 不满足 | Session 创建时读取快照并固定依赖，没有 User Turn/TurnConfig 边界 |
| 模型热切换 | 不满足 | `ModelAdapter` 在 Session 创建时固定 solver/coordinator |
| 多 Session 并发 | 已有基础 | 共享 Host/Endpoint，独立 Kernel、Runtime、record 与 UI 状态 |
| Workspace 工具边界 | 已有基础 | `bone-tools` 已 canonicalize 并阻止路径逃逸 |
| Workspace 稳定身份 | 不满足 | 只有路径字符串，没有跨平台 WorkspaceRegistry 与持久 UUID |
| Session 跨重启恢复 | 不满足 | Session ID 是进程内递增整数，没有 SessionStore、durable ack 或 continuation context |
| Session 进程并发 | 不满足 | 没有 per-session writer/Runtime lease，也没有只读打开和接管语义 |
| Slash Command registry | 不满足 | Composer 仅字符串识别 `/stop`、`/exit` |
| 配置损坏 TUI 修复 | 不满足 | build/snapshot/connect 错误在 TUI 前返回 |
| 凭据独立存储 | 部分满足 | credential 与普通配置已分离，但连接生命周期独占锁阻止多 Workspace 并发，需要 CredentialBroker/短时 refresh lock |
| 真实模型目录 | 不满足 | Endpoint 能按 ID 创建模型，但没有模型 listing 接口 |
| One-shot 共用 SessionStore | 不满足 | 当前 one-shot 可导出观察日志，但不会创建可在 TUI 继续的 durable SessionRecord |

## 16. 发布门禁

以下任一情况存在时，不得宣称完成本产品目标：

- 首次使用仍要求复制 `config.example.json`；
- 缺少或损坏配置仍会在 TUI 前退出；
- 存在公开设置只能手工编辑 JSON；
- `/model` 只写配置但运行时不更新；
- 模型设置在一个 User Turn 中途生效，导致同一任务混用 TurnConfig；
- 修改设置后仍要求 restart、reload 或重建 Session；
- UI 无法区分 Desired、Effective、Last Known Good 或 PendingNextTurn；
- 设置范围不清楚或静默覆盖其他 Session；
- Session override 没有“恢复继承”能力；
- Session 在退出 BONE 后丢失；
- 用户消息在 durable acknowledgement 前被清空或启动 Agent；
- 相同 Workspace 无法恢复历史 Session；
- 恢复只显示 UI 历史但后续模型没有 continuation context；
- 不同 Workspace 的 Session 会混在一起；
- 同一 Session 可被两个进程同时写入；
- 第二个 Workspace 因第一个进程持有 credential 长锁而无法运行；
- slash command 意外进入模型上下文；
- 用户无法安全发送 `/` 开头的普通消息或 paste 会触发命令；
- UI 显示“已应用”但底层使用旧 revision；
- 安全配置损坏后自动采用更宽松权限；
- 尚无 authoritative listing 的 catalog 候选被标记为“当前账号可用”；
- 失败会清空草稿、历史或旧的有效设置。

## 17. 参考产品决策

本 PRD 参考成熟 coding agent 已验证的交互惯例，但不机械复制其产品边界：

- [Codex developer commands](https://developers.openai.com/codex/cli/slash-commands)：可发现的 slash command、`/model` 与 `/status`；
- [Codex configuration](https://developers.openai.com/codex/config-basic)：用户级与项目级配置来源；
- [Claude Code configuration](https://code.claude.com/docs/en/configuration)：配置问题的交互恢复与诊断；
- [Claude Code sessions](https://code.claude.com/docs/en/sessions)：项目范围内的 Session 保存与恢复；
- [Gemini CLI commands](https://github.com/google-gemini/gemini-cli/blob/main/docs/reference/commands.md)：typed command、模型与设置入口；
- [Gemini CLI session management](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/session-management.md)：项目隔离的 Session 持久化；
- [OpenCode configuration](https://opencode.ai/docs/config/)：最小配置、分层设置与认证分离；
- [OpenCode TUI](https://opencode.ai/docs/tui/)：Session、模型和连接的 TUI 操作；
- [Aider commands](https://aider.chat/docs/usage/commands.html)：键盘友好的命令补全和错误提示。

BONE 明确不采用以下假设：Git root 自动定义 Workspace、单会话运行时、`/model` 静默修改全局默认值，以及 Markdown 聊天文件作为多 Session 主存储。
