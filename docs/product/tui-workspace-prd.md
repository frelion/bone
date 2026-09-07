# BONE TUI 配置、Workspace 与多 Session PRD

| 字段 | 内容 |
| --- | --- |
| 文档状态 | 当前实现基线 + 后续体验设计参考 |
| 目标版本 | SQLite `BoneStore` 重构完成后的 BONE TUI |
| 产品优先级 | P0 |
| 最后更新 | 2026-09-07 |
| 产品范围 | 启动体验、TUI 配置、实时配置、Workspace、Session、Slash Command |
| 核心原则 | 持久化对用户不可见；SQLite 是 BONE 自有数据的唯一真相来源 |

> **当前实现优先级（2026-09-07）**
>
> 本节是当前工程与产品的生效契约，优先于本文后面保留的较早体验草案。BONE 不再有用户可编辑的配置文件、`bone-config`、`ConfigManager`、`ConfigSection`、`ConfigTool`、`BONE_CONFIG`、`BONE_STATE_DIR` 或 `credential_root`。不要将本文中较早的 JSON、JSONL、文件锁、`ConfigService`、`Desired/Effective/LKG`、配置 revision 或运行时热切换文字理解为当前实现行为；它们仅保留为未来产品设计素材，须重新设计后才可实现。
>
> 当前持久化与运行时边界如下：
>
> - BONE 自有设置、Workspace、Session、草稿、状态和会话事件历史只写入一个 SQLite 数据库。默认路径为 `$XDG_DATA_HOME/bone/store-v1/bone.sqlite3`，未设置 XDG 时为 `~/.local/share/bone/store-v1/bone.sqlite3`。数据库使用 WAL、`synchronous = FULL`、foreign keys 和 fail-fast Busy 语义；它不是用户设置入口，也不应手工编辑。
> - App 在启动时只打开一次通用 `BoneStore`，并由 App 自己定义 settings、Workspace 和 Session 的 typed key 与记录。`documents` 和 `journal_entries` 是存储内部表；业务代码仅使用 typed `Document<T>`、`Journal<E>`、受限 transaction 与 lease，不能自行拼 SQL、路径或任意 key。
> - `GlobalSettings` 保存用户默认 Solver；`WorkspaceSettings` 保存工作目录默认 Solver；`SessionRecord` 保存当前会话 override。解析顺序固定为 **Session override > Workspace default > User default**。没有模型时正常进入 `NeedsModel`，不会猜测模型。
> - `/model` 的保存立即持久化到其选择的 scope。已 attached 的 runtime 保持原来的不可变 `ResolvedAgentRuntimeConfig`；只有新建或重建 runtime 才解析新值。本轮没有 `/config` Settings Center、文件 watcher、跨进程设置通知或运行中热切换。
> - 交互 TUI 启动时传入的初始模型会写入其打开的 Session override；one-shot 的 `--model` / `BONE_MODEL` 仅对该次调用生效，不创建或持久化 `SessionRecord`。
> - 每个已接受的用户消息在同一个 SQLite transaction 中写入 `UserTurnAccepted` journal fact 与 Session summary/state。提交失败时不得清空 Composer 或启动 Agent。SQLite document revision 只是不透明的乐观并发 token，不是面向用户的“配置版本”。Session writer lease 是 fail-fast OS lock；其他进程可只读打开该 Session。
> - ChatGPT OAuth 是唯一的 JSON 例外：Rig 持有 schema 和 refresh 生命周期，App 的 `ChatGptCredentials` 只提供经权限检查的 `ChatGptAuthLease`。其私有 cache 默认在 `$XDG_CONFIG_HOME/bone/store-v1/providers/chatgpt-subscription/`（无 XDG 时 `~/.config/bone/store-v1/providers/chatgpt-subscription/`）。secret 永不进入 SQLite、journal、诊断或 TUI。活跃 Endpoint/Model 仍持有 lease 时，`/logout` 必须返回 Busy，不能删除 cache。
>
> 历史本地数据不会被读取、迁移、覆盖或删除；新版本从新的 `store-v1` 根开始。`--events` 的 JSONL 是 one-shot 观察导出，不是 BONE 的 durable store，也不得被拿来恢复 Session。

## 1. 执行摘要

用户在任意目录执行 `bone` 后，直接进入一个可操作、可诊断、可恢复的 TUI。用户无需预先创建或查看任何设置文件，无需复制示例 JSON，也无需知道内部 document key、SQLite 路径或配置 section。

启动时的精确当前目录，经规范化后成为本次 BONE 实例不可变的 Workspace。一个 Workspace 可以拥有多个相互独立、并发运行、可跨重启恢复的 Session。当前 `/model` 可在 TUI 内持久化模型选择；模型、推理强度和 Agent 行为在 runtime 创建时冻结，不能在一个正在执行的用户任务中途换模型。完整 Settings Center 与更广泛的设置命令是后续工作，不是本轮实现承诺。

一句话产品定义：

> 精确启动目录定义不可变 Workspace；Workspace 包含多个持久化 Session；BONE 在未选择模型、未登录或存储异常时仍可进入 TUI；当前模型选择由 TUI 完成，已运行任务保持其启动时冻结的配置。

## 2. 背景与问题

### 2.1 当前实现基础

BONE 当前已经拥有：

- 全屏、响应式、多 Session TUI；
- 一个 `AgentHost` 共享认证连接，多个 Session 独立运行；
- 每个 Session 独立的草稿、历史、任务、滚动位置与未读状态；
- 基于启动 `cwd` 的工具访问边界；
- 一个 App 生命周期内共享的 `BoneStore`，以 SQLite 保存 typed settings、Workspace、Session 和 journal；
- SQLite transaction 把 durable user-turn acceptance 与 Session state 作为一个提交边界；
- 以 document revision 做乐观并发控制、以 OS lease 做 Session writer 与 ChatGPT cache ownership；
- 不可变的 `ResolvedAgentRuntimeConfig`，由 App 在启动 runtime 前解析并注入 Agent；
- 后台 Session 持续工作且不会抢走当前焦点。

### 2.2 已解决的历史用户问题与后续缺口

旧流程曾要求用户手动准备配置，并会在进入 TUI 前读取 `agent.system`、认证和连接服务。这一阻断路径已被 SQLite `BoneStore` 与 `NeedsModel` 状态取代：没有模型也能打开 Workspace、Session 与草稿。存储损坏、权限错误或 schema 不匹配不会自动重置数据，而应进入 TUI 的 repair/error 状态。

下列条目是旧架构的问题，其中一部分已通过本轮实现解决，其他部分仍是后续体验工作：

- 正常用户曾必须理解和编辑 JSON；现在不再有这一产品路径；
- 示例模型 ID 只是占位字符串，却可能通过本地校验；
- 登录信息显示在全屏 TUI 之外；
- 已运行 Session 不会热切换；这是当前刻意的 pinned-runtime 语义，而非文件读取缺陷；
- 模型实例在 Session 创建时固定；
- slash command 只有硬编码的 `/stop` 和 `/exit`；
- Session ID 只在当前进程内有效，退出后无法恢复；
- 当前事件导出是观察日志，不是可靠的恢复存储。

因此，本项目不是“增加一份默认配置”和“补几个命令”，而是建立 BONE 的产品级 App Shell、响应式设置系统、Workspace 身份和持久化 Session 模型。

## 3. 产品愿景与体验原则（第 0 节当前实现基线优先）

### 3.1 持久化对普通用户隐形

- 正常用户从安装到长期使用都不需要打开任何持久化文件。
- 不存在“只有手工编辑 JSON 才能完成”的公开设置。
- SQLite 路径、内部 document key、schema 和 document revision 只可在技术诊断中出现。
- 不提供手工配置文件这一开发者逃生通道；测试、portable embedding 与未来 host 通过 App composition 显式注入数据根路径。

### 3.2 TUI 始终是恢复入口

以下情况不能阻止 TUI 启动：

- store 尚不存在；
- 尚未选择模型；
- SQLite store 损坏、权限异常或 schema 不匹配；
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

### 3.4 当前修改即保存，状态必须真实

- 设置项不需要“保存全部”按钮。
- 当前 `/model` 成功后立即作为 typed document 写入 SQLite；CAS conflict 或 Busy 会返回可见错误，不能冒充成功。
- 已 attached runtime 保持启动时冻结的完整 `ResolvedAgentRuntimeConfig`。界面应说明新模型用于下一次 runtime 创建或重建，而不是宣称已经改变正在运行的任务。
- `Desired/Effective/Last Known Good` 三阶段设置状态、运行时 apply acknowledgement 和热切换不是当前行为；后续实现必须在不破坏上述 pinned-runtime 语义的前提下重新设计。

### 3.5 作用范围用人话表达

面向用户只使用：

- 仅当前对话；
- 当前工作目录；
- 所有工作目录。

内部可分别映射到 Session、Workspace、User scope。

### 3.6 实时不破坏进行中的工作

本轮的“实时保存”只表示 mutation 立即提交 SQLite，不表示热切换。一次用户消息触发的完整 Agent 工作周期称为一个 User Turn；Agent 使用 App 在启动 runtime 前解析的不可变 `ResolvedAgentRuntimeConfig`。正在执行的 runtime 不更换模型；新建或重建 runtime 才使用当前 Session/Workspace/User 覆盖后的值。展示设置可以在下一帧更新，但不能用它推断运行时已经切换。

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

## 6. 核心概念与产品边界（当前存储/运行时规则以第 0 节为准）

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
opaque document revision（仅并发控制，不在普通 UI 展示）
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

### 6.3 User Turn 与不可变 Runtime Config

一条已持久化并被接受的用户消息启动一个 User Turn。Turn 可以包含多次模型调用、工具调用和 Coordinator 判断，直到完成、等待用户、停止或中断。

App 在接受用户消息前解析完整配置，并在启动/重建 runtime 时锁定 `ResolvedAgentRuntimeConfig`：

```text
solver model and effort
coordinator model and effort
agent/system behavior
model deadlines
model-visible tool definitions
```

上述值在同一 runtime/Turn 内不变化。Turn 进行时的 `/model` 可以立即保存到 SQLite，但不得改变已 attached runtime；新建或重建 runtime 才解析该值。本轮不维护 `PendingNextTurn`、`Effective` 或 Last Known Good 的用户可见状态。

### 6.4 当前模型设置作用域

有效优先级：

```text
Current Session override
> Workspace settings
> User settings
```

`GlobalSettings.agent.default_solver` 是用户默认值；`WorkspaceSettings.default_solver` 是工作目录默认值；`SessionRecord.solver_model_override` 是当前对话覆盖。`/model` 可写入这三个 scope；`/model inherit` 删除 Session override 并恢复 Workspace/User 继承。没有隐式环境变量层，也不会猜一个内建模型。

下面的 descriptor/Settings Center 设计是后续产品工作，不是当前存储 API：

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

当前解析结果仅需包含：

```text
solver/coordinator 模型与 effort
tool limits 与 runtime deadlines
完整 resolved runtime fingerprint
来源 scope（仅诊断）
```

### 6.5 已取代的 Desired、Effective 与 Last Known Good 设计

以下三阶段是保留的未来 UX 设计，不是当前 `BoneStore` 或 TUI 的行为：

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
→ 无模型时显示 `NeedsModel`
→ 用户在 TUI 内执行 `/model <id>`、`/model default <id>` 或 `/model global <id>`
→ 在需要工作时连接 ChatGPT 并惰性附着 Agent Runtime
→ 发出第一条任务
```

要求：

- 不出现设置文件步骤；
- 当前需要用户提供模型 ID；catalog/picker 与推荐模型是后续工作；
- `/model` 的 scope 必须明确，不得静默覆盖既有 User/Workspace 值；
- TUI 首帧不等待网络；
- 用户可以先写草稿；
- 如果用户确认发送，登录成功后才自动提交；
- 取消登录时完整恢复草稿。

### 7.2 切换当前 Session 模型

```text
/model <id>
→ 默认范围为“仅当前对话”
→ 校验模型选择格式
→ 持久化 Session override
→ 显示“已 attached runtime 保持 pinned；新建或重建 runtime 使用此模型”
```

### 7.3 修改当前 Workspace 默认值

```text
/model default <id>
→ 修改“当前工作目录”的默认模型
→ SQLite transaction 保存
→ 继承 Workspace 设置的 Session 在下一次 runtime 创建时解析新值
→ 有独立覆盖的 Session 保持不变
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

### 7.5 Store 故障

```text
执行 bone
→ TUI 仍然出现
→ 显示“存储需要修复”
→ 保留 SQLite database、WAL 和 SHM
→ 不自动 reset、删除、覆盖、迁移或猜测替代设置
→ 用户可以查看诊断、重试或安全退出
```

### 7.6 One-shot CLI

本 PRD 的 App Shell 和 Overlay 交互仅适用于无消息参数的交互模式。当前 one-shot 只复用 Store 中的全局设置解析、provider auth 与 Agent runtime 接线；它不创建或持久化 Session：

```text
bone <message>
bone --model <id> <message>
bone --events <path> <message>
```

- `bone <message>` 不创建 `SessionRecord`；
- `--model` / `BONE_MODEL` 是该次调用的 ephemeral override，不写 Session/Workspace/User 默认值；
- 已有有效认证和模型时直接执行；
- 首次使用且 stdin/stdout 为 TTY 时，提示用户先运行无参数 `bone` 完成 TUI 设置，不降级为手改文件；
- 非 TTY 且缺少认证或模型时，以结构清楚、可执行的错误退出，不尝试设备登录，不打印 secret；
- `--events` 是外部观察导出，不替代 SQLite 内的 durable Session journal；
- **未来设计，不是当前实现：**让 one-shot 创建可在 TUI 中继续的 durable Session；
- 交互 TUI 启动的初始模型可写入打开的 Session override；TUI 后续设置可以覆盖它。

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

### 8.2 当前存储架构与故障语义

| ID | 优先级 | 需求 |
| --- | --- | --- |
| ID | 优先级 | 当前实现契约 |
| --- | --- | --- |
| STORE-001 | P0 | 首次交互式启动自动创建 SQLite store、schema、全局 settings 与 Workspace/Session record；不猜模型、不自动登录 |
| STORE-002 | P0 | BONE 自有业务数据只有 `bone.sqlite3` 一个 source of truth；没有 JSON/JSONL 双写、session index 文件或用户设置文件 |
| STORE-003 | P0 | typed `Document<T>` 以 revision CAS 更新；短 `BEGIN IMMEDIATE` 写 transaction 遇到竞争立即返回 Busy，TUI 不得阻塞 reducer |
| STORE-004 | P0 | 同一 transaction 内写入 accepted user-turn journal fact、Session summary 和 active-turn state；任一步失败整体 rollback |
| STORE-005 | P0 | 数据库损坏、权限异常或 schema 版本不匹配不得自动 reset、删除、迁移或覆盖旧数据；TUI 进入 storage repair/error 状态 |
| STORE-006 | P0 | Session writer lease 与 provider auth lease 是 fail-fast OS lock，不承担普通 document 写入锁；lease 冲突有可见 Busy/只读状态 |
| STORE-007 | P0 | token、OAuth code、API key 与 ordinary SQLite documents、journal、日志和诊断完全隔离 |
| STORE-008 | P0 | 历史文件格式、外部手改持久化文件、自动 backup/recovery/import 和跨进程 watcher 都不属于本轮 |

当前物理存储职责：

```text
User data directory
  └── `bone/store-v1/bone.sqlite3`
      ├── global settings, workspace registry and workspace settings
      └── session records, drafts, state and journals

Private provider config directory
  └── `bone/store-v1/providers/chatgpt-subscription/auth.json` (Rig-owned opaque cache)

Workspace directory
  └── no implicit BONE internal data
```

### 8.3 后续：TUI 设置中心

> 本节是保留的交互设计，不是当前命令面。当前可用的设置入口是 `/model`、`/model default <id>`、`/model global <id>` 和 `/model inherit`；不得将 `/config`、descriptor registry 或 JSON 设置编辑器视为已实现功能。

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

### 8.4 当前模型保存与 runtime 生效边界

| ID | 优先级 | 需求 |
| --- | --- | --- |
| LIVE-001 | P0 | `SettingsService` 解析 typed Global/Workspace/Session values，生成完整的 immutable `ResolvedAgentRuntimeConfig` 和 fingerprint |
| LIVE-002 | P0 | `/model` 的 scope mutation 通过 App-owned durable service 立即提交；success 只表示已持久化，Busy/CAS conflict 必须可见 |
| LIVE-003 | P0 | 已 attached runtime 不订阅设置变化，也不从磁盘重读配置；它始终使用启动时注入的完整 runtime config |
| LIVE-004 | P0 | 新建或重建 runtime 解析当前覆盖链；Session override 的清除立即恢复 Workspace/User 继承 |
| LIVE-005 | P0 | journal 记录实际 solver 与完整 resolved runtime fingerprint，避免接受消息与启动 runtime 读到不同配置 |
| LIVE-006 | P1 | Settings Center、运行时 apply acknowledgement、跨进程设置通知、watcher 和 runtime hot switch 另行设计，不得借旧文件 revision 旁路 `BoneStore` |

当前生效边界：

| 设置 | 生效时间 |
| --- | --- |
| `/model` Session / Workspace / User mutation | SQLite commit 后立即成为下一次 runtime 解析的输入 |
| 已 attached runtime 的 solver、coordinator、limits、deadline | 保持启动时的 `ResolvedAgentRuntimeConfig`，不热切换 |
| 新建或重建 runtime | 使用当时解析的覆盖链并计算新 fingerprint |
| ChatGPT auth cache | Endpoint/Model 持有 provider lease；活跃 lease 存在时 logout 返回 Busy |
| Workspace root | 当前实例不可修改 |

### 8.5 模型选择

当前命令面不依赖 catalog 或 Settings Center。`/model` 不带参数时只说明可用语法；调用方提供的模型 ID 做领域格式校验，Provider 能力的最终验证发生在建立 runtime/请求时。本节后面的 catalog、picker 和缓存要求是未来体验设计。

| ID | 优先级 | 需求 |
| --- | --- | --- |
| MODEL-001 | P0 | `/model` 无参数说明当前可用语法；Model Picker / provider catalog 是后续工作 |
| MODEL-002 | P0 | `/model <id>` 默认作用于当前 Session，避免意外影响其他对话 |
| MODEL-003 | P0 | `/model <id>` 修改当前 Session Solver；已 attached runtime 保持 pinned，新建或重建 runtime 使用保存的选择 |
| MODEL-004 | P0 | `/model default <id>` 修改当前 Workspace 的默认 Solver |
| MODEL-005 | P0 | `/model global <id>` 修改用户级默认 Solver |
| MODEL-005A | P0 | `/model inherit` 删除当前 Session override，使其重新继承 Workspace/User 默认模型 |
| MODEL-006 | P0 | 普通 `/model` 不修改 Coordinator；Coordinator 位于 Models 的高级设置 |
| MODEL-007 | P0 | 本地仅验证模型选择的领域格式；Provider/账号兼容性在连接/runtime 请求时由实际结果报告 |
| MODEL-008 | P0 | 失败不得清空草稿或伪造已切换；错误应保持可操作 |
| MODEL-009 | P1 | authoritative model catalog、缓存 freshness、搜索和 picker 另行设计 |
| MODEL-010 | P0 | 当前只支持 ChatGPT subscription Provider，不展示未实现的 Provider/endpoint 切换 |

以下 model catalog 来源与可信度规则是未来设计，不是当前 `/model` 实现：

| 来源 | UI 标记 | 可否直接称为当前账号可用 |
| --- | --- | --- |
| Provider 返回的当前账号 authoritative listing | 已验证 | 可以 |
| 当前会话最近一次成功 listing 缓存 | 已验证于某时间 | 仅在显示缓存时间后可以沿用 |
| BONE 版本化兼容 catalog | 尚未验证 | 不可以，只能作为候选并在提交前做 Provider 能力验证 |
| 用户手工输入 | 未验证 | 不可以，且只在高级入口出现 |

未来 P0 若提供 `ModelCatalog`，必须遵循以下规则；当前不得通过一次会计费的普通生成请求伪装成 listing。刷新失败时：

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

- 用户设置、WorkspaceRegistry、Workspace settings、Session records 与 journals 通过 SQLite 的短 `BEGIN IMMEDIATE` transaction 和 document revision/CAS 更新；遇到并发写立即返回 Busy，不在 TUI reducer 中等待；
- Session journal 每个 Session 只有一个 writer，获得 fail-fast OS Session writer lease 后才能接受新消息；
- 同一 Session 已被其他进程使用时，当前进程可以只读查看、返回该实例，或经明确确认请求接管，不能双写；
- 当前 lease 是 ownership lock，不是 SQLite transaction 或通用文档写锁；其失效由进程退出释放，当前版本不实现 fencing-token 接管协议；
- 同一 Workspace 的不同 Session 可以并发持有各自 journal lease；
- `ChatGptAuthLease` 在 `auth.lock` 上独占，Endpoint 与其派生 Model handle 存活期间一直持有；同一 BONE cache 的第二个连接立即返回 Busy；
- `/logout` 只能经 `ChatGptCredentials::clear` 删除本地 Rig OAuth cache。若仍有活跃 lease 则返回 Busy；成功只清除本地 cache，不宣称 revoke 远端会话。

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
| SES-012 | P0 | 用户消息只有在同一个 SQLite transaction 中写入 `UserTurnAccepted`、Session summary/state 并成功 commit 后才显示 Accepted、清空 Composer 并启动 Agent Turn |
| SES-013 | P0 | 启动仅加载 Session index；历史按需 hydrate，Runtime 按需 attach，不为全部历史 Session 建立模型运行时 |
| SES-014 | P0 | 同一 Session 的可写 Runtime lease 互斥；锁冲突时只读打开、定位已有实例或显式接管 |
| SES-015 | P0 | 恢复后提交的新消息必须获得已完成历史的模型可见上下文，而不仅是恢复 UI timeline |
| SES-016 | P0 | Runtime 数量受资源预算约束；Working Runtime 不被静默驱逐，Idle/Complete Session 只有在 durable 后才可 detach；预算耗尽时新 Turn 明确进入 QueuedForRuntime，不能静默停止其他 Session |

恢复是冷恢复：历史和上下文可以继续，进程内 Future、网络流和正在运行的外部命令不会跨进程继续。Session rail 默认展示当前 Workspace 的 Active Session 元数据；选择 Session 时 hydrate，提交新消息时 attach Runtime。`/sessions` 打开当前 Workspace 的 Session picker，`/resume` 是选择并 hydrate/attach 一个持久 Session，不代表恢复旧 Future。Archived Session 默认不出现在 rail，可在 picker 中筛选。

#### 8.7.1 Durable acknowledgement 与 RPO

“已确认用户消息”具有严格定义：

```text
Composer text
→ begin SQLite write transaction
→ append `UserTurnAccepted` to per-session journal
→ update Session summary / active-turn state
→ commit according to durable store contract
→ return durable acknowledgement
→ clear Composer and mark Accepted
→ start User Turn
```

规则：

- durable acknowledgement 只发生在 journal 与 Session state 的同一个 SQLite transaction commit 后；SQLite 使用 WAL 与 `synchronous = FULL`。RPO 0 的故障域覆盖正常退出、进程崩溃和强制终止，不承诺存储硬件损毁；
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
historical actual solver and resolved runtime fingerprint metadata
```

恢复语义：

- 已完成 Turn 可进入后续模型上下文；
- 未完成 Turn 以明确的 Interrupted boundary 结束，只保留可证明完成的消息和工具结果；
- 不重新执行历史工具或副作用；
- 新建或重建 runtime 时使用当前已解析的配置；历史保留其实际 solver 与 runtime fingerprint 元数据；
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
| AUTH-003 | P0 | `/logout` 经 `ChatGptCredentials::clear` 清除本地 Rig OAuth cache；若任何 Endpoint/Model 仍持有 `ChatGptAuthLease`，返回 Busy 而不删除任何文件 |
| AUTH-004 | P0 | logout 成功不宣称 revoke 远端会话；已发出的请求由其现有连接语义完成或失败，不能在一次请求中静默换账号 |
| AUTH-005 | P0 | SessionRecord、历史和草稿保持本地可见；新账号首次向旧 Session 发起 Turn 前再次确认数据将发送给新账号 |
| AUTH-006 | P0 | device code、token 和 authorization header 不进入持久 transcript、普通日志、诊断导出或模型上下文 |

`/logout` 不删除 Session。若当前有 Working Session，provider lease 使 logout 直接失败为 Busy；界面必须说明先停止或退出持有连接的实例后再试，草稿与历史仍保留。用户取消确认时不得改变 credential 或连接状态。

### 8.10 状态、诊断与恢复

| ID | 优先级 | 需求 |
| --- | --- | --- |
| DIAG-001 | P0 | `/status` 展示 Workspace、当前 Session、已解析模型来源、attached runtime 的 pinned solver（如有）、认证与连接状态 |
| DIAG-002 | P0 | 存储诊断检查 SQLite store、WorkspaceRegistry、SessionStore/lease、ChatGPT cache 私有目录权限和连接；`/config doctor` 是未来 Settings Center 的设计，不是当前命令 |
| DIAG-003 | P0 | 默认错误回答：发生了什么、用户数据是否安全、现在可以做什么 |
| DIAG-004 | P0 | 技术路径、内部 document key、opaque revision 与错误链仅在“技术详情”中展示，且不得泄露 OAuth payload |
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

当前状态展示必须区分已持久化的模型选择与已 attached runtime 的 pinned 值：新值写入成功后可提示“新建或重建 runtime 时使用”，不能显示为已热切换。`Desired/Effective/Last Known Good`、`Current/Next turn` 双版本状态属于后续 Settings Center 设计。

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
- UI 把 SQLite 保存误称为 runtime 热切换的已知事件为 0；
- 未知副作用自动重放次数为 0；
- 跨 Workspace 静默重新绑定次数为 0；
- 同一个 Session 同时出现两个 journal writer 的次数为 0；
- 同一账号打开第二个 Workspace 因 provider auth lease 冲突失败的次数按当前 lease 语义可见报告，不得静默等待或损坏 auth cache。

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
- 模型默认不能自主修改用户设置。未来若开放设置工具，必须逐次显示 diff、scope 和影响并获得用户批准，并只能经 App-owned typed domain service 修改；
- 引入写工具前必须实现 Workspace 写协调、SQLite 事务/lease 冲突处理或可选 worktree 隔离。

### 10.5 存储、容量与锁

- WorkspaceRegistry、settings、Session records 和 journal 都在同一个 SQLite source of truth 中；普通写用短 transaction，不持有进程生命周期文件锁；
- Session journal 以严格 sequence 的 SQLite rows 保存；transaction rollback 后不得留下半条 event，单个 logical Session 的错误不得污染其他 Workspace；
- SQLite corruption、权限错误或 schema mismatch 不得自动删除、reset 或迁移数据库；
- 磁盘不足时提前显示持久警告；一旦无法 durable append，不接受新消息、不清空 Composer；
- Archive 只改变可见性，不等同于释放空间；
- P1 设置页提供存储占用、导出、可恢复删除和 compact 管理；P0 不得自动删除用户 Session；
- Session/provider lease 是 OS lock；当前版本不通过超时偷锁，也不实现自动接管。显式接管作为后续协议必须先展示可能的运行中任务和副作用风险。

## 11. 历史成功指标草案（后续 UX 版本重审）

以下指标引用 `/config`、Effective revision、catalog 或热切换时，均描述未来体验目标，不能作为当前 SQLite `BoneStore` 行为或发布承诺。

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

## 12. 历史验收草案（非当前实现契约）

本节保留原始产品验收素材，便于后续 Settings Center、catalog、跨进程通知与热切换重新立项时使用。它不覆盖第 0 节，尤其是其中涉及配置文件、JSON、`/config`、`Desired/Effective/LKG`、配置 revision、文件 watcher、自动迁移、one-shot durable Session 或 runtime 热切换的场景均不是当前行为。

当前存储重构的验收要点是：干净 store 自动创建且不猜模型；`/model` 三层继承正确；Session user-turn 与 journal/state 原子提交；失败不清 Composer/不启动 Agent；不同 Workspace 隔离；Session writer/provider auth lease Busy 语义正确；OAuth secret 不进入 SQLite；SQLite 错误进入 repair/error 而不 reset 数据。

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

## 13. 历史风险与缓解素材（第 0 节优先）

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

## 14. 历史分阶段交付草案（不描述当前完成状态）

下列 roadmap 保留为未来产品拆分参考。涉及旧文件配置、`ConfigService`、CredentialBroker、Desired/Effective/LKG、ModelCatalog 或 runtime hot switch 的条目必须以 `BoneStore` 和 pinned runtime 为基础重新设计，不可按原文直接实施。

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

## 15. 当前实现快照

| 产品能力 | 当前状态 | 已实现边界 / 后续缺口 |
| --- | --- | --- |
| TUI-first 与未选模型 | 已实现 | Store、Workspace、Session 和草稿可在 `NeedsModel` 下打开；不猜模型、不自动登录 |
| BONE 自有持久化 | 已实现 | 一份 bundled SQLite `BoneStore`；没有 user-visible config JSON、JSONL durable transcript 或第二份 session index |
| 原子 durable acceptance | 已实现 | `UserTurnAccepted` 与 Session summary/state 在同一 SQLite transaction 中 commit；失败不清 Composer/不发 Agent |
| 模型三层继承 | 已实现 | Session override > Workspace default > User default；`/model inherit` 清除 Session override |
| 已运行 runtime 行为 | 已实现且刻意 pinned | `ResolvedAgentRuntimeConfig` 在 runtime 创建时冻结；不做 runtime hot switch |
| 多 Workspace / 多 Session | 已实现 | Workspace identity、Session records 和 journals 存于 SQLite 并按 Workspace 隔离 |
| 同 Session writer ownership | 已实现 | fail-fast OS writer lease；冲突走 Busy/只读语义，不用 SQLite transaction 代替 ownership |
| Provider OAuth | 已实现 | Rig-owned opaque `auth.json` 由 App-owned `ChatGptAuthLease` 保护；logout 有活跃 lease 时 Busy |
| 存储故障 | 已实现基础 | Busy、CAS conflict、权限、corruption、schema mismatch 不自动 reset；TUI 应进入 repair/error 状态 |
| 完整 Settings Center / catalog | 后续 | 当前 `/model` 四种 scope 命令可用；`/config`、picker、catalog、watcher 与跨进程 notification 尚未实现 |
| one-shot durable Session | 后续 | one-shot 不创建 `SessionRecord`；`--model` / `BONE_MODEL` 是本次调用的 ephemeral override；`--events` 仅观察导出 |

## 16. 当前发布门禁

以下任一情况存在时，不得宣称完成当前 SQLite 存储重构；后续 UX 目标应另立版本门禁：

- 首次使用仍要求复制或手工编辑设置文件；
- store 不存在、未选择模型、未登录时 TUI 在首帧前退出；
- BONE 自有数据出现 JSON/JSONL 双写或第二份 Session index source of truth；
- `/model` 未按 Session > Workspace > User 存储/解析，或声称热切换已 attached runtime；
- Agent 在启动时重新从用户存储读取配置，导致 accepted turn 的 attribution 与 runtime 配置漂移；
- 设置范围不清楚或静默覆盖其他 Session；
- Session override 没有“恢复继承”能力；
- Session 在退出 BONE 后丢失；
- 用户消息在 durable acknowledgement 前被清空或启动 Agent；
- 相同 Workspace 无法恢复历史 Session；
- 恢复只显示 UI 历史但后续模型没有 continuation context；
- 不同 Workspace 的 Session 会混在一起；
- 同一 Session 可被两个进程同时写入；
- provider auth lease、Session writer lease 或 SQLite Busy 被静默吞掉、无限等待或用于错误的并发语义；
- slash command 意外进入模型上下文；
- 用户无法安全发送 `/` 开头的普通消息或 paste 会触发命令；
- store 损坏、权限异常或 schema mismatch 自动删除、reset、覆盖或迁移旧数据；
- OAuth secret 写入 SQLite、journal、debug output、TUI notice 或 model-visible output；
- failed storage mutation 清空草稿、历史或启动 Agent；
- `/logout` 在活跃 Endpoint/Model 仍持有 lease 时删除 OAuth cache。

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
