# Testing：系统化验证策略

BONE 的测试目标是证明三个 crate 之间的边界和关键状态交错，而不是追求测试数量。默认测试必须确定、离线、无真实凭据，并能在失败时指出哪个不变量被破坏。真实 provider、进程崩溃和长会话成本使用明确的专项入口。

## 测试金字塔

| 层 | 主要证明 | 替身边界 |
| --- | --- | --- |
| Core Kernel | 输入 routing、Job 权限、Context、等待、终态与迟到结果 | 直接驱动 Event；不启动 Tokio task |
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

- 相同 / 冲突 Input ID、回答问题的 sequence 检查和新输入合并 routing；
- root create/update、委派原子性、owner 权限、调查只读和 Job depth / capacity；
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
- replay、tool correlation、terminal Response 和 stream state machine；
- read / glob / grep 的 path、UTF-8、ignore、binary、遍历与输出 limits；
- apply_patch grammar、context matching、stage / commit / rollback 和 concurrent change；
- Bash deadline、stdout / stderr bound、environment 与 Unix process-group cleanup。

这层允许真实临时目录、文件和子进程，因为它们正是工具的生产依赖。测试完成后不得访问 workspace 外部状态或依赖用户 shell 配置。

Provider contracts 位于 [`crates/bone-adapters/tests/`](../crates/bone-adapters/tests/)，fixture 位于 [`fixtures/`](../crates/bone-adapters/tests/fixtures/)，共享 recording transport 位于 [`support/`](../crates/bone-adapters/tests/support/)。

| Contract | 覆盖范围 |
| --- | --- |
| `model_contract.rs` | endpoint、protocol、model identity 和公开边界 |
| `openai_responses_contract.rs` | `/responses`、headers、reasoning、tools、replay、SSE terminal、usage 与错误 |
| `openai_chat_completions_contract.rs` | `/chat/completions`、tools、replay、SSE terminal、usage 与错误 |
| `anthropic_messages_contract.rs` | `/v1/messages`、thinking / text、tools、SSE terminal、cache usage 与错误 |
| `chatgpt_subscription_contract.rs` | Codex Responses URL、auth / headers、forced SSE/body、replay、identity 与 redaction |
| `tools_configuration.rs` | ToolEnvironment defaults、serialized limit 和 workspace configuration |

Request JSON 按解析后的语义比较，不把对象 key 顺序当契约。fixture 只保存稳定的 provider 输入输出，不包含 real token、device code、OAuth payload 或完整生产错误。test transport 只补 Rig test doubles 没有记录的 metadata，并且只在 `test-utils` feature 下编译。

Provider contract 不测试 BONE 生产路径没有启用的 Rig option，也不复制验证第三方内部实现。需要兼容差异时，先用一个离线 fixture 复现，再添加最小 typed option。

## App integration

App tests 使用临时 data directory、真实 `bone.sqlite3`、真实 journal / CAS / lease 和公开 `App` / `Session` API。只有不可确定或会访问网络的模型边界使用脚本替身。

它们应覆盖：

- canonical Workspace identity、Session 创建 / 打开 / archive 和 writer lease；
- submit 的事务回执、RequestId 幂等 / 冲突和输入状态；
- 未选模型、登录缺失、lazy Runtime startup、retry 与 stop 顺序；
- watch snapshot + history cursor 的无丢失组合，包括空公开页但 cursor 前进；
- 三层配置解析、字段级并发更新、生效屏障、失败后 suspended 和重试；
- Runtime ID 对 stale Job / Call / Question control 的隔离；
- Runtime Record 归档、存储失败后重放以及 restart 的 Interrupted / Queued 区分；
- 写入前记录、Workspace gate、未知写阻塞、核查、close 与 shutdown 报告；
- profile、API-key slot 和 ChatGPT auth lease 的 ownership。

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

TUI 自身随后增加三类测试：纯 reducer 状态表、宽 / 窄布局 snapshot，以及用真实 App test backend 驱动的少量交互链路。终端字节解析与 Agent 业务不变量不在 TUI 重测。

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
