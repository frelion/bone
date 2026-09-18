# Testing：系统化验证策略

BONE 的测试目标是证明三个 crate 之间的边界和关键状态交错，而不是追求测试数量。默认测试必须确定、离线、无真实凭据，并能在失败时指出哪个不变量被破坏。真实 provider、进程崩溃和长会话成本使用明确的专项入口。

## 测试金字塔

| 层 | 主要证明 | 替身边界 |
| --- | --- | --- |
| Core Kernel | 持续会话、Job 权限、Context、等待、显式结案与迟到结果 | 直接驱动 Event；不启动 Tokio task |
| Core Runtime | 端口调度、timeout、取消、reconfigure、shutdown 与资源释放 | 脚本化 ModelPort / ToolPort |
| Adapters unit | request / response value、工具算法、path 与 limit | 内存值或临时 workspace |
| Provider contract | BONE 经过生产协议转换后发送和接收的真实 wire shape | 离线 recording transport + fixture |
| App integration | 公开 App / Session 行为、SQLite、lease、配置和写事实 | 真实临时文件与 SQLite；模型 / 网络在最外层替换 |
| Public API | 外部 crate 能否只靠公开 API 编译与使用 | `crates/bone-app/tests/public_api.rs` |
| Live certification | 某真实 endpoint 当前仍接受声明的协议 | 显式网络、凭据和可能费用 |

越靠近底层状态机，场景应越短、交错越精确；越靠近产品入口，用例越少，但要保留真实 assembly。相同业务不变量无需在每一层复制：Kernel 测状态裁决，Runtime 测异步执行，App 测持久与产品语义。

## Core Kernel

Kernel tests 位于 [`crates/bone-core/src/tests.rs`](../crates/bone-core/src/tests.rs)。它们直接创建 Kernel、提交输入和模型 / 工具完成 Event，再检查 Effect、Record 与 `AgentView`。

这层负责覆盖：

- 相同 / 冲突 Input ID、回答问题的 sequence 检查和新输入使旧会话提案失效；
- 问候直接回复而不创建 Job，Job 完成不自动完成输入，只有会话明确结案；
- root create/update、委派原子性、固定工具权限和 Job depth / capacity；
- 每个 Worker 只得到自己的 Context、显式 evidence 授权、目录与 Record 分页；
- 完整序列化预算、checkpoint 覆盖范围、大工具结果的有界预览与继续分页；
- 工具完成恰好发生在 `Await::Tool` 提案提交前后的竞态；
- pause、cancel、constraint / revision 变化和迟到模型提案；
- child、Inquiry、未读投递和未知外部写对 Finish 的门禁；
- stop 后终态不复活、完成唯一、写核查幂等。

一个回归测试应复现最小合法事件序列，并断言最终权威状态。不要通过调用私有 helper 直接构造运行时不可能出现的破损状态，除非测试对象就是恢复 / 校验逻辑。

对有限状态域，优先使用表驱动测试；对并发竞态，显式排列两种 Event 顺序。只有当生成式测试能定义可靠 oracle 时才引入 property testing，当前不需要为 Job DSL 增加新框架。

## Core Runtime

Runtime 单元测试位于 [`runtime.rs`](../crates/bone-core/src/runtime.rs)，完整行为仍通过 Core 的 crate tests 观察。它们使用实现 `ModelPort` / `ToolPort` 的小型脚本替身。

这层只证明异步边界：

- 每个 Effect 启动一次正确的 port 方法；
- Call timeout、panic、取消与 caller drop 被转换成一次终态；
- waiting Job 释放 Worker slot，交互 root 的保留槽可用；
- `suspend → reconfigure → resume_scheduling` 保留 Job 图，在途工具继续使用旧 port；
- `shutdown_grace`、未知写报告和最后 handle drop；
- broadcast lag 后 baseline 仍能恢复事实。

测试必须使用明确的 `reached` / `release` channel 或 barrier 表达先后关系。`yield_now`、短 sleep 和“现在应该还没结束”的负断言不能证明任务已经走到目标观察点。deadline 逻辑优先使用 Tokio paused time；真正依赖 OS process 的测试才使用现实时间，并设置宽裕上限。

## Adapters：值、协议与工具

普通 unit tests 与实现放在同一模块，证明 BONE 自己的转换和算法：

- `EndpointConfig`、`ModelOptions`、Request validation 和身份 / secret redaction；
- terminal Response、stream state machine 和模型调用只走 streaming 的边界；
- read / glob / grep 的 path、UTF-8、ignore、binary、遍历与输出 limits；
- apply_patch grammar、context matching、stage / commit / rollback 和 concurrent change；
- Bash deadline、stdout / stderr bound、environment 与 Unix process-group cleanup。

这层允许真实临时目录、文件和子进程，因为它们正是工具的生产依赖。测试完成后不得访问 workspace 外部状态或依赖用户 shell 配置。

`Model::stream` 是唯一的模型调用入口，这条边界由三层固定：`llm/model.rs` 只把 Rig 的 `stream` 擦除进 `Model`（`exposes_one_bone_completion_path` 证明一次 BONE 调用只触发一次 provider 调用并恰好得到一个 terminal，`incomplete_stream_is_an_explicit_terminal_error` 证明没有 terminal 的 EOF 是显式 `IncompleteStream`）；`llm/protocol/` 的三个 constructor 通过 test-only `ScriptedStreamingClient` 回放 SSE fixture，断言真实 method、URL、headers 与 body；`agent/model.rs::streamed_text_is_reported_as_bounded_progress_records` 证明 delta 只合并成有界的进度记录，决策仍来自 terminal Response。`Request` 只带 BONE 真正发送的字段：`agent/model.rs::request_carries_the_bound_contract_specific_choice_and_configured_options` 核对强制 submission tool 与已验证 options，provider contracts 用 `body["max_tokens"].is_null()` / `body["max_output_tokens"].is_null()` 固定「调用方没有设置的上限就不上线」。replay 面不存在：`InputItem` 只有带来源归属的文本一种形态，协议层也没有把响应转回输入的入口，因此测试不再构造 tool result 或跨轮身份核对场景。

Provider contracts 位于 [`crates/bone-adapters/tests/`](../crates/bone-adapters/tests/)，fixture 位于 [`fixtures/`](../crates/bone-adapters/tests/fixtures/)，共享 recording transport 位于 [`support/`](../crates/bone-adapters/tests/support/)。

| Contract | 覆盖范围 |
| --- | --- |
| `model_contract.rs` | endpoint、protocol、model identity 和公开边界 |
| `openai_responses_contract.rs` | `/responses`、headers、reasoning、tools、SSE terminal、usage 与错误 |
| `openai_chat_completions_contract.rs` | `/chat/completions`、tools、SSE terminal、usage 与错误 |
| `anthropic_messages_contract.rs` | `/v1/messages`、thinking / text、tools、SSE terminal、cache usage 与错误 |
| `chatgpt_subscription_contract.rs` | Codex Responses URL、auth / headers、forced SSE/body、identity 与 redaction |
| `tools_configuration.rs` | ToolEnvironment defaults、serialized limit 和 workspace configuration |

Request JSON 按解析后的语义比较，不把对象 key 顺序当契约。fixture 只保存稳定的 provider 输入输出，不包含 real token、device code、OAuth payload 或完整生产错误。test transport 只补 Rig test doubles 没有记录的 metadata，并且只在 `test-utils` feature 下编译；它只服务流式路径，unary 调用直接失败，而不是被一个空的成功 body 掩盖。

Provider contract 不测试 BONE 生产路径没有启用的 Rig option，也不复制验证第三方内部实现。需要兼容差异时，先用一个离线 fixture 复现，再添加最小 typed option。

## App integration

App tests 使用临时 data directory、真实 `bone.sqlite3`、真实 journal / CAS / lease 和公开 `App` / `Session` API。只有不可确定或会访问网络的模型边界使用脚本替身。

它们应覆盖：

- canonical Workspace identity、Session 创建 / 打开 / archive 和 writer lease；
- submit 的事务回执、RequestId 幂等 / 冲突和输入状态；
- 未选模型、登录缺失、lazy Runtime startup、retry 与 stop 顺序；
- watch snapshot + history cursor 的无丢失组合，包括空公开页但 cursor 前进；
- 三层配置解析、字段级并发更新、保存后通知、显式 reload 应答、失败后 suspended 和重试；
- Runtime ID 对 stale Job / Call / Question control 的隔离；
- Runtime Record 归档、存储失败后重放以及 restart 的 Interrupted / Queued 区分；
- 写入前记录、Workspace gate、未知写阻塞、核查、close 与 shutdown 报告；
- profile、API-key slot，以及 ChatGPT cache 的跨进程 refresh / invalidate / logout 事务；
- 删除连接：不存在的 id 返回 `Ok` 且不产生第二份事实，只移除指定连接并保持其余顺序，ChatGPT cache 随删除一起清掉，仍选择它的 Session 报告 `ConfigProblem::MissingProfile` 并保留原选择。见 `tests.rs::deleting_a_profile_rewrites_the_config_and_keeps_the_others_in_order`、`deleting_an_unsaved_profile_is_a_noop_that_leaves_the_config_alone`、`deleting_a_chatgpt_profile_drops_its_cached_credentials`、`deleting_a_profile_leaves_selecting_sessions_reporting_a_missing_profile`，以及 `file_config.rs::delete_profile_rewrites_the_file_and_keeps_the_remaining_order`、`delete_profile_is_idempotent_for_an_unsaved_id`、`deleting_an_unsaved_profile_does_not_create_the_config_file`。

CAS 并发测试必须让所有 contender 使用同一个初始 revision，并用 barrier 同时起跑；断言严格一个成功、其余为 conflict，再读取最终 value。测试生产查询时要调用真实生产方法，不能复制一份 SQL 后只证明那份测试 SQL 正确。

happy path helper 必须返回并核对具体 outcome。`Completed` 场景不能把 Failed / Cancelled / Rejected 统称为“任意终态”。存储故障测试应在明确的 durability boundary 注入错误，并分别断言未发布假确认、可重放事实和 unresolved write 状态。

离线完整装配测试从 App Profile / RuntimeConfig 开始，经过真实 `ProviderConnector → ConfiguredModel → ModelAdapter` 和离线 transport，最终观察 `InputFinished(Completed)` 与重开后仍完整的持久 history。它只替换凭据读取与 Endpoint 获取；系统凭据和默认 HTTP client 的创建继续由各自专项测试覆盖。只在 `RuntimeBackend::Ports` 注入 ModelPort 的测试仍有价值，但不能替代这条生产组装验证。

## 恢复、长会话与 TUI 门禁

以下专项场景是 TUI 开始承载真实工作前的发布门禁：

1. 子进程在写入意图保存后、外部效果发生后、工具结果保存后和 Agent Record 归档前分别退出；重启后不自动重复写，Session lease 与 unresolved query 给出正确事实。
2. 消费者落后于 watch、Core broadcast lag、历史页只有被过滤位置、关闭再打开时，公开事件没有丢失或重复。
3. 随累计 Record / Session journal 增长，测量 `observe` / snapshot、history page、submit acknowledgement 和 Runtime bootstrap。性能优化必须由这些数据支持。
4. Linux、macOS 和 Windows / WSL 的 path、private permission、lease、SQLite 与 process cleanup 在声称支持的平台实际运行。

这些场景不应塞进每次毫秒级 unit suite。崩溃测试使用独立 helper process 和临时目录；长会话先作为显式 measurement，建立稳定环境和阈值后再决定是否进 CI。

TUI 自身随后增加三类测试：纯 reducer 状态表、宽 / 窄布局 snapshot，以及用真实 App test backend 驱动的少量交互链路。PTY 门禁还必须覆盖初始化查询和正常运行中的终端 EOF（`pty_terminal.rs::closing_the_terminal_during_initialization_or_input_exits`），以及草稿保存失败或超时后的不可逆退出与终端恢复（`pty_terminal.rs::draft_failure_or_timeout_never_reopens_the_ui_after_quit`）。终端字节解析与 Agent 业务不变量不在 TUI 重测。

`/model` 面板是「连接 = tab」结构，契约测试至少覆盖下表条目；键位列就是 [`tui-platform-architecture.md`](tui-platform-architecture.md) 5.6 节冻结键位表的可执行版本：

| 契约 | 断言 | 位置 |
| --- | --- | --- |
| tab 条只列已保存连接 | 打开面板时 tab 条等于已保存 `Profile` 加末尾 `+ Add connection`；未保存的连接类型不占 tab，只在 kind picker 里出现 | `state/overlay.rs::the_tab_strip_walks_connections_and_clamps_at_the_add_tab`、`the_add_tab_has_no_models_and_its_last_row_opens_the_kind_picker` |
| tab 开窗与裁剪 | 当前 tab 始终在窗口内且可点；单个 label 比整条更宽时被裁剪而不是丢弃 | `view/overlay.rs::every_connection_and_the_add_tab_own_a_click_target`、`a_windowed_strip_keeps_the_current_tab_visible`、`a_label_wider_than_the_strip_is_clipped_and_still_clickable` |
| 左右 / 上下分工 | 左右只在 Tab 屏产生 `PreviousTab` / `NextTab` 并在两端 clamp；上下的行移动在 Tab、Kind 与 Reasoning 保留 | `input/keymap.rs::tab_arrows_move_between_connections_only_on_the_tab_screen`、`the_tab_screen_keeps_the_vertical_row_keys`、`the_kind_picker_keeps_one_vertical_route_to_every_connection_kind` |
| tab 内的模型可见性 | 每个 tab 只展示属于它的模型：目录项在前，连接自己保存但目录未发布的模型在后，且不会串到相邻 tab | `state/overlay.rs::a_stale_connection_never_shows_a_manual_model_from_another_connection`、`a_reload_keeps_the_tab_inside_the_strip_and_lands_a_new_connection_on_its_own_tab` |
| 末行手工输入 | 末行 Enter 进 `ModelInput`；`ModelText` / `ModelBackspace` / `ModelClear` 只作用于当前 tab，空白输入只报 status，合法 ID 才变成 `ModelSelection` | `state/overlay.rs::the_manual_model_editor_belongs_to_the_current_tab`；`input/keymap.rs::the_manual_model_editor_mirrors_the_form_keys`；`view/connection.rs::the_manual_model_editor_shows_its_value_and_an_apply_action` |
| `e` 与 `d` 只作用于连接 | add tab 上没有编辑器动作；`e` 进 Setup（ChatGPT 进 Login）；`d` 进 `ConfirmDelete` | `state/overlay.rs::the_add_tab_has_nothing_to_edit_or_delete`、`editing_a_chatgpt_tab_restarts_sign_in_without_a_form`；`input/keymap.rs::connection_editing_keys_stay_out_of_text_fields` |
| 删除确认 | `ConfirmDelete` 只接受 `y` / `n` / Esc；确认后对话框立即关闭并恰好记一次 `ModelOperationKind::Delete`，第二次确认与 Esc 都被拒绝，迟到回报按 `request` 丢弃 | `state/overlay.rs::a_confirmed_delete_records_its_request_and_closes_the_dialog`；`input/keymap.rs::delete_confirmation_answers_yes_no_or_escape`；`view/connection.rs::the_delete_confirmation_names_the_connection_and_offers_y_and_n` |
| 删除结果 | 成功回 Tab 第一行并重新加载目录；失败只留一条面板 status，面板随即恢复可用 | `state/overlay.rs::a_failed_delete_stays_visible_and_leaves_the_panel_usable`；`view/overlay.rs::a_pending_delete_names_itself_and_keeps_the_strip` |
| 移除已保存模型 | `Delete` 只移除连接保存过的模型；目录里出现但连接没保存过的模型没有可移除对象，只报 status | `state/overlay.rs::a_saved_model_is_removed_from_its_tab_and_the_strip_reloads`、`a_model_the_connection_never_saved_cannot_be_removed`；`view/connection.rs::the_removal_form_only_offers_removal` |
| reasoning 门禁 | ChatGPT 订阅与 OpenAI Responses 的连接在选中模型或保存带模型的连接时先开 `Reasoning`，其余协议直接应用 | `state/overlay.rs::selecting_a_responses_model_opens_reasoning_before_apply`、`saving_a_connection_with_a_responses_model_chooses_reasoning_first` |
| 已连接类型不重复索要凭据 | kind picker 标注 `Already connected · opens its tab`，选择它跳到已有 tab 而不重开表单 | `state/overlay.rs::choosing_a_saved_official_kind_opens_its_tab_instead_of_asking_again`；`view/connection.rs::the_kind_picker_names_a_connection_that_already_has_a_tab` |
| 添加与登录 | 从 add tab 走完 form 或登录后回到 tab 条并重新加载目录；失败的表单保留输入并要求重输密钥 | `state/overlay.rs::adding_a_connection_from_the_add_tab_returns_to_the_tab_strip`、`the_kind_picker_starts_chatgpt_login_without_inventing_a_model`、`signing_in_returns_to_the_tab_strip_and_reloads_the_strip`、`saved_key_failure_returns_to_models_without_requesting_the_secret_again`；`view/connection.rs::failed_key_save_prompts_reentry_instead_of_suggesting_blank`、`pending_save_shows_progress_without_an_action_or_caret` |
| 迟到回执 | 只有匹配当前在途操作的回执才生效：切换 Session 或重开面板后到达的 load / save / login 回执必须同时匹配 session 与 request，delete 回执按 request + kind 结算自己的 pending；不匹配的回执不改状态 | `state/overlay.rs::stale_load_receipts_cannot_finish_or_mutate_a_reopened_panel`、`late_connection_receipt_preserves_a_new_form_and_its_status`、`cancelled_login_reloads_saved_catalogue_and_rejects_its_late_receipt`、`session_switch_dismisses_login_without_changing_focus` |
| secret 边界 | API key 只经 `SecretText`（`Debug` 输出 `[redacted]`）进入 `SetupText`，model ID 是普通 `String` | `view/connection.rs::connection_form_masks_key_and_keeps_all_fields_and_actions_inside_small_screens`；`input/mod.rs::setup_keyboard_ownership_controls_fields_and_secret_paste`、`connection_form_keys_and_paste_use_secret_safe_actions`、`command_chords_do_not_become_model_text`；`input/keymap.rs::the_manual_model_editor_mirrors_the_form_keys`；`state/connection.rs` 的 `SecretText` |
| 键盘与指针一致 | 点击直接派发该行的 action 但不夺取键盘所有权；键盘仍在 workspace 时打字与粘贴进 Composer，未修饰 Enter 或 `F6` 才把所有权交给面板，两条路径产生同一个 `Effect` | `tests/interaction_contract.rs::model_panel_has_one_keyboard_truth_and_an_independent_pointer_route`、`mouse_model_overlays_keep_input_visible_and_controls_within_their_surface`；`input/mod.rs::mouse_panel_keeps_typing_and_paste_in_composer_until_f6` |

keymap 表在 `input/keymap.rs` 中按屏幕逐格断言「有 action / 返回 `None`」，不复制 reducer 逻辑：左右键在 ModelInput、Kind、Setup、Reasoning、ConfirmDelete、Login 上都必须是 `None`，`y` / `n` / `d` / `e` 只在各自允许的屏幕生效，并拒绝带修饰符的近似按键。

## 断言与替身规范

一个有效测试至少包含可观察前因和明确结果：

- 用公开 outcome、Record、View、持久数据或实际文件变化作为 oracle；
- 对“不发生”先证明系统已经到达可能发生该动作的决策点，再释放竞争方；
- 对 idempotency 同时检查返回值和没有第二份事实 / 效果；
- 对失败检查稳定 enum kind 和状态，只有消息本身是契约时才比较完整字符串；
- 对 bounded output 同时检查大小、truncated / cursor 和未破坏 UTF-8；
- 对 secret 检查 Debug、error、fixture 和 journal，而不是只检查正常 Response。

替身只替换被测层外面的不确定边界。它应使用脚本化的期望输入和有限响应，不在内部重新实现待测算法。测试 helper 若隐藏重要终态、自动重试或时间顺序，就应拆小。

以下测试通常应删除或合并：

- 与另一个用例经过相同生产路径、只重复同一成功断言；
- 复制生产 SQL、schema 或转换逻辑再验证复制品；
- 只断言“至少一个成功”“某个终态”“调用没有立刻完成”；
- 针对不可构造的内部错配状态增加的防御性测试；先用类型消除该状态；
- 直接打开产品从未启用的第三方 feature，却被当作 BONE 产品覆盖。

不同执行层的相似测试不因名字相同自动合并。Kernel reconfigure 证明状态转换，Runtime reconfigure 证明 task 和 port 生命周期，App reconfigure 证明 durable barrier；三者 oracle 不同。

## 常规执行

完整本地 / CI 检查：

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
cargo test --workspace --doc --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked
```

`--all-features` 很重要：它会编译并执行 `bone-adapters` 的离线 provider contracts。依赖已下载时可以加 `--offline`，但 CI 的正确性不依赖开发机 cache。

聚焦迭代：

```sh
cargo test -p bone-core --all-targets --all-features --locked
cargo test -p bone-adapters --all-targets --all-features --locked
cargo test -p bone-app --all-targets --all-features --locked
```

本地 Rig patch 有独立门禁：

```sh
cargo test --manifest-path third_party/rig-core-0.42.0/Cargo.toml \
  --no-default-features --features reqwest,rustls,test-utils \
  --locked --lib providers::chatgpt
cargo test --manifest-path third_party/rig-core-0.42.0/Cargo.toml \
  --no-default-features --features reqwest,rustls,test-utils \
  --locked --lib atomic_json_write_replaces_complete_private_record
```

Linux CI 运行全 workspace fmt、clippy、all-targets / all-features tests、doctests 和 rustdoc。macOS 与 Windows 至少编译整个 workspace 的所有 target / feature；平台特有行为应逐步从 compile gate 提升为实际 tests。

## Live certification

live tests 默认 `#[ignore]`，因为它们访问网络、需要 secret 且可能收费。只在明确选择 endpoint 和 model 时执行：

```sh
export OPENAI_API_KEY='...'
export BONE_OPENAI_MODEL='...'
# optional: export OPENAI_BASE_URL='https://gateway.example/v1'
cargo test -p bone-adapters --test live_openai_responses -- --ignored --nocapture

export OPENAI_API_KEY='...'
export BONE_OPENAI_CHAT_MODEL='...'
cargo test -p bone-adapters --test live_openai_chat_completions -- --ignored --nocapture

export ANTHROPIC_API_KEY='...'
export BONE_ANTHROPIC_MODEL='...'
# optional: export ANTHROPIC_BASE_URL='https://gateway.example'
cargo test -p bone-adapters --test live_anthropic_messages -- --ignored --nocapture
```

live probe 只断言稳定结构：非空文本、恰好一个 terminal、provider-resolved identity 和完整 Response；不固定自然语言措辞。ChatGPT subscription 的 wire behavior 使用离线 contract，交互 device login 由宿主显式调用 `App::login`，不在自动 live suite 中弹出。

[`.github/workflows/provider-live.yml`](../.github/workflows/provider-live.yml) 只允许手工触发，从 GitHub secrets / variables 读取配置。任何 secret、device code、refresh token 或 OAuth payload 都不得进入 fixture、snapshot、test output、认证记录或模型可见消息。

## 新行为的测试完成定义

每次修复或功能至少回答四个问题：

1. 最低层能稳定复现的回归在哪里？
2. 哪个公开或跨层测试证明 production assembly 没有漏接？
3. 失败路径、取消 / 重试和持久边界是否改变？
4. 是否已有相同 oracle 的测试可以删除或合并？

覆盖率用于寻找未经过的分支，不作为单一发布分数。对 CAS、旧 Call 资格、Context budget 和外部写效果等关键机制，可以偶尔做小范围 mutation：故意移除校验，确认对应测试确实失败。
