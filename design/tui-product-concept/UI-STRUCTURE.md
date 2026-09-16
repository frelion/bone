# BONE TUI · 界面结构与组件设计

本文件回答：有哪些页面、区域和组件；它们放在哪里、负责什么；用户操作后界面如何变化；如何分步实现。2026-09-10，本轮产品设计提案。产品能力按完整范围设计，实现顺序仅表示依赖关系。

## 1. 顶层结构

工作台由顶部全局栏、左侧会话栏、中部会话工作区、右侧详情区、底部操作提示栏组成。右侧默认关闭，所以常态是两栏。输入框属于中部会话工作区，不跨整个窗口。

```text
AppShell
├── GlobalBar                     顶部，始终存在
├── WorkspaceBody
│   ├── SessionRail               左侧，宽度不足时收起
│   ├── MainSurface               中部，根据主视图替换内容
│   │   ├── SessionWorkspace      默认：会话工作区
│   │   │   ├── SessionHeader
│   │   │   ├── ConversationView
│   │   │   └── Composer
│   │   ├── AttentionView         全局待处理事项
│   │   ├── SessionBrowser        会话搜索、归档与管理
│   │   └── SettingsView          设置
│   └── DetailPane                右侧，按选中对象展开
├── ActionBar                     底部，当前焦点的可用操作
└── DialogLayer                   临时选择或明确确认
```

```text
┌────────────────────── GlobalBar ──────────────────────────┐
├─ SessionRail ─┬──────── MainSurface ────────┬─ DetailPane ─┤
│               │ SessionHeader              │ DetailHeader │
│               ├────────────────────────────┤ DetailBody   │
│               │ ConversationView           │              │
│               │                            │              │
│               ├────────────────────────────┤              │
│               │ Composer                   │              │
├───────────────┴────────────────────────────┴──────────────┤
│ ActionBar                                                │
└──────────────────────────────────────────────────────────┘
```

切到 SettingsView 或 AttentionView 时，只替换中部内容，保留工作区身份与会话选择。中部不是会话页时不展示会话输入框，避免用户误发消息。返回会话后恢复原草稿、滚动位置与详情选择。

## 2. 一级区域的明确职责

| 区域 | 固定内容 | 可执行操作 | 不能承担的职责 |
| --- | --- | --- | --- |
| GlobalBar | 产品名、工作区名称、需要你数量、设置入口 | 回工作台、打开待处理列表、进入设置 | 不承载当前任务的停止、验收或工具状态 |
| SessionRail | 新建、会话列表、搜索与归档入口 | 切换、新建、进入会话管理 | 不放子任务树、文件树或 Agent 全量列表 |
| MainSurface | 当前主视图 | 会话交流、管理会话、处理事项或设置 | 一次只承载一个主视图 |
| DetailPane | 当前对象标题、分类、详细内容、关闭 | 切详情分类、读取来源、返回上层对象 | 不独立切换会话，不被后台新事件换掉内容 |
| ActionBar | 当前焦点位置及最多几个常用动作，更多入口 | 显式选择当前动作、移动到更多操作 | 不作为重要功能的唯一入口，不同时列所有快捷键 |
| DialogLayer | 单次选择或确认的标题、说明、动作 | 选择、确认、取消 | 不承载长篇结果、设置首页或全文证据阅读 |

GlobalBar 与 ActionBar 各以一行为起点。SessionRail 约 22–26 列；DetailPane 约 36–44 列；中部使用剩余宽度。正文使用一致字符尺寸，宽度不足时切换区域布局，不缩小正文。

## 3. 会话工作区：核心组件

### SessionHeader / 会话标题栏

显示会话名称、一句总体进展以及“查看详情 / 更多操作”。总体进展优先显示需要回答、执行中断等影响当前工作的状态；普通工作显示简洁进展。模型、运行编号等不堆在标题旁。

点击名称进入重命名小对话框。更多操作含会话归档、停止当前会话工作等，并明确作用范围。此处不提供含糊的“停止”来代指某个子任务。

### ConversationView / 会话正文

它是可滚动的内容容器，包含以下固定种类的条目。每条有稳定身份，后台更新不改变用户的阅读锚点。

| 条目组件 | 展示内容 | 展开或操作结果 |
| --- | --- | --- |
| UserMessage | 用户原文、时间、保存或提交状态 | 长内容展开；提交失败保留可恢复原文 |
| AssistantMessage | 面向用户的完整答复 | 长内容展开；不把回复出现当作请求完成 |
| WorkGroup | 当前要求对应的任务概览、各项简短状态 | 选中 WorkItemRow，在右侧打开 TaskDetail |
| ToolSummary | 工具名、用途、执行结果摘要 | 右侧打开 ToolDetail，长输出分页阅读 |
| QuestionBlock | 问题、必要背景、明确选项或回答入口 | Composer 进入回答模式并绑定问题 |
| ResultBlock | 结果说明、产物入口、验证摘要 | 打开 ArtifactDetail 或 AcceptanceDetail |
| SystemNotice | 中断、配置影响等对用户有意义的边界 | 打开对应恢复或设置页面 |

ConversationView 底部另外包含 ActivityLine，显示当前正在执行或整理的活动。它是临时展示，不作为已保存的历史条目。用户向上阅读时显示 NewContentMarker，选择后才回到底部。

### WorkGroup / 工作概览

默认展示目标及几项任务，任务名优先于 Agent 名称。每行 WorkItemRow 显示任务名、状态与必要的等待原因。任务较多时显示“查看全部工作”，进入详情区内的 TaskTree，而不是让中央无限拉长。

TaskTree 显示父子任务关系。选中节点展示目标、范围、完成条件、当前进展和相关结果；暂停、恢复、取消针对所选任务，并标明作用范围与可用性。全局会话栏不因新增子任务增长。

### Composer / 输入区

内部由 ReplyContext、DraftEditor、SubmissionFeedback 和 ComposerFooter 组成。

- ReplyContext：仅回答问题时出现，显示问题摘要与取消回答。
- DraftEditor：多行编辑，独立保存每个会话的草稿。默认将输入发给当前会话。
- SubmissionFeedback：保存中、保存失败、已保存等待配置等明确反馈。仅收到持久回执后清除对应已发送文本。
- ComposerFooter：发送、附件或引用入口（需要产品支持）、模型设置入口。只有存在明确可停止工作时才显示相应操作。

没有模型、未登录、执行中断时，输入仍可编辑。发送过程中允许编辑下一条内容，但不能重复发送同一请求。回复失效问题时保留回答，显示“作为新消息发送”。

## 4. 右侧详情：容器统一，内容按对象切换

DetailPane 由 DetailHeader、DetailNavigation、DetailBody 组成。标题必须标明当前对象。可以在“工作 / 变更 / 上下文 / 记录”之间切换，但无关分类不伪装成可用功能；空内容解释原因。

| 详情组件 | 内部组件与内容 | 入口 |
| --- | --- | --- |
| TaskDetail | TaskSummary、TaskTree、TaskStatus、TaskActions | WorkItemRow 或查看工作分工 |
| ArtifactDetail | ArtifactList、FileDiffView / TextArtifactView、ValidationSummary | ResultBlock、变更入口 |
| ContextDetail | SessionBackground、TaskMaterials、RecentRequirements、SourceList | 上下文分类 |
| RecordDetail | RecordSummary、SourceMetadata、RawRecordView | 记录分类、来源链接 |
| ToolDetail | ToolRequestSummary、ToolOutcomeSummary、OutputReader | ToolSummary |
| AcceptanceDetail | DeliverySummary、CriterionList、EvidenceReader、AcceptanceActions | 检查验收条件 |
| DecisionDetail | DecisionReason、KnownFacts、MissingEvidence、DecisionActions | 待处理事项 |

SourceList 和 EvidenceReader 可复用来源条目：标题、来源、时间和查看原文。长差异与完整记录可进入主区的专注阅读页，保留“返回结果 / 返回背景”，并在返回时恢复原位置。

详情只在用户选择时切换对象；后台状态更新可以刷新当前对象的数据，不能跳到另一个对象。已经删除或失效的目标显示说明并允许返回。

## 5. 其他主视图

### AttentionView / 需要你

由 AttentionHeader、AttentionList、AttentionItemRow 组成。列表项显示事项标题、所属会话、为何需要用户及严重性。选中后右侧显示 DecisionDetail；窄屏则进入详情页。

分类包括待回答问题、阻塞工作的配置或权限问题、未知外部影响，以及需用户判定的结果。普通运行和普通新消息不进入这里。处理后列表更新并保留稳定位置；列表为空时显示“目前没有需要你处理的事项”和返回工作入口。

### SessionBrowser / 会话管理

由 SearchField、SessionFilter、SessionList、SessionActions 组成。支持当前工作区内搜索与归档筛选，新建、重命名和归档使用明确会话目标。选择会话返回 SessionWorkspace。

SessionRail 与 SessionBrowser 复用 SessionRow：名称、简短状态、未读标记、选中态。未读与待回答采用不同文字含义。Busy 会话进入说明页，提供刷新与返回，不能冒充已打开可写会话。

### SettingsView / 设置

由 SettingsNavigation 与 SettingsSection 组成，分类为模型、连接、运行设置与诊断。每个可覆盖设置包含 ScopeSelector、DesiredValue、RunningValue、InheritanceHint 与 ApplyFeedback。

连接设置使用 ProfileList、ProfileEditor、CredentialStatus 和 LoginDialog。密钥不回填完整值；登录由用户主动发起。登录中、成功、失败与取消均在登录区域表达；完成后回到原设置，再返回会话草稿。

设置属于主视图，长表单不塞进窄侧栏。修改失败要说明保存选择与当前生效选择是否不同，不能假装整体已回滚。

## 6. 临时界面与通用组件

临时界面包括 ModelPicker、ActionMenu、RenameDialog、LoginDialog、ConfirmationDialog。动作菜单必须显示名称和说明，支持从可见入口进入；快捷键是辅助提示。

通用视觉组件：TextAction、StatusLabel、SelectionRow、SectionHeading、InlineNotice、SourceRow、EmptyState、LoadingState、ErrorState。状态标签同时使用短文字；风险颜色只用于相应语义。普通内容使用连续文本，减少每段独立边框。

ConfirmationDialog 使用明确动作名称，如“退出并保留记录”。返回或关闭只撤销当前层操作；停止、取消任务与退出必须有各自可见入口。高风险核查先在详情中读依据，最终判定才使用确认层。

## 7. 区域切换与状态归属

| 状态 | 归属范围 | 切换后如何处理 |
| --- | --- | --- |
| 当前主视图、当前会话 | 全局工作台 | 显式导航时改变 |
| 草稿、正文滚动位置、未读 | 每个会话 | 切会话保持独立，返回恢复 |
| 当前任务或资料选择、详情分类 | 每个会话的展示状态 | 进入设置不清空，返回恢复 |
| 当前对话框、调用入口焦点 | 当前临时操作 | 关闭恢复原焦点；目标失效时回到有效入口 |
| 输入、工作、历史、配置实际状态 | 产品公开事实 | 界面订阅更新，不能用本地猜测替代 |

正文阅读、列表导航、输入编辑和详情阅读是四个焦点区域。ActionBar 只展示当前区域的少量动作。“更多操作”能发现其余功能。任何后台事件都不能把焦点从正在编辑的文字移开。

## 8. 宽窄布局规则

宽度阈值需要真实终端验证。初始方向：160 列允许左中右；120 列常态左中，打开详情可收起左栏成为中右；80 列单主区，通过“会话 / 详情”入口导航；40 列继续单区，长内容换行或进入专注阅读。

右栏转为独立页时复用同一详情内容与选中对象，不重写一套功能。高度不足时优先减少外围留白和展开内容，保留输入与退出路径。窗口恢复后不丢草稿和阅读位置。

## 9. 实现依赖顺序

| 批次 | 组件范围 | 做完后可直接验收的完整行为 |
| --- | --- | --- |
| 1：空间骨架 | AppShell、GlobalBar、SessionRail、MainSurface、ActionBar、DialogLayer | 左中右区域切换、焦点移动、详情开关与窄屏返回都可演示 |
| 2：会话闭环 | SessionHeader、ConversationView、基础消息、Composer、SessionBrowser | 创建、切换、保存输入与恢复阅读位置形成完整路径 |
| 3：工作呈现 | WorkGroup、TaskDetail、TaskTree、ActivityLine、QuestionBlock、ToolDetail | 工作中补充要求、回答问题、查看分工与控制明确任务 |
| 4：结果与资料 | ResultBlock、ArtifactDetail、ContextDetail、RecordDetail、AcceptanceDetail | 从结果到变更、背景、来源与验收逐层进入并返回 |
| 5：全局与恢复 | AttentionView、DecisionDetail、SettingsView、LoginDialog、退出确认 | 配置问题、外部核查、登录、保存失败、中断与退出有完整处理路径 |

每批都包含自身的空、加载、失败、失效与窄屏状态。批次表示实现依赖，不是发布范围，也不意味着前两批就是最终产品。

## 10. 数据契约缺口

当前代码基线为 56920ca3。App 已提供会话、输入、问题、工作、活动、历史、配置与写入核查等基础能力。以下组件的数据需求需要单独补齐：

- ContextDetail：会话摘要、任务摘要、来源关系与原文阅读；不能直接把内部所有记录当成公开背景。
- AcceptanceDetail：独立的用户验收、部分接受与保留风险的判定事件。
- ArtifactDetail：结构化产物目录、文件差异与验证依据；工具完成事件不自动等同于这份目录。
- 精确的要求审阅或采纳状态：已接收不足以证明，必要时需要明确产品事实。
- 定向任务输入、附件与引用提交：现有 SubmitInput 只公开文本与问题引用，相关交互不得声称已经接通。

组件名称用于界面分工，不强制对应 Rust 文件、crate 或框架类型。布局和组件内容按本稿评审；技术组织在产品边界清楚后确定。
