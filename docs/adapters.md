# Adapters：模型协议与本地工具

`bone-adapters` 是 BONE 的基础设施边界。它包含两个彼此独立的能力集合：

- `llm` 把 provider wire protocol 收敛成一次 `Request → ResponseStream`；
- `tools` 提供 workspace 内的原生读、搜索、补丁和进程工具。

crate root 的 `ModelAdapter` 和 tool-port adapters 再把它们接到 `bone-core::ModelPort` / `ToolPort`。Adapters 不拥有 Session、配置存储、凭据位置、授权政策或另一套 Agent loop。

## LLM 边界

调用方先取得 `Endpoint`，再选择 `Model`，最后提交一份完整有序的 `Request`。`Model::stream` 是唯一的模型调用入口，只想要最终结果的调用方把 stream 消费到 terminal 事件：

```rust,no_run
use bone_adapters::llm::{
    InputItem, InputSource, Request, StreamEvent,
    protocol::openai_responses,
};
use futures_util::StreamExt;

# async fn run(api_key: String) -> Result<(), Box<dyn std::error::Error>> {
let endpoint = openai_responses::official("primary", api_key)?;
let model = endpoint.model("model-id")?;
let mut stream = model.stream(
    Request::new([InputItem::external(InputSource::User, "Hello")])
        .instructions("Answer briefly"),
).await?;

let mut completed = None;
while let Some(event) = stream.next().await {
    if let StreamEvent::Completed(response) = event? {
        completed = Some(response);
    }
}

println!("{}", completed.expect("one terminal response").text().unwrap_or_default());
# Ok(())
# }
```

这三个身份不能混用：

| 身份 | 作用 |
| --- | --- |
| endpoint ID | App 分配的连接身份，用于诊断和凭据绑定 |
| protocol | OpenAI Responses、OpenAI Chat Completions 或 Anthropic Messages 的 wire 语义 |
| model ID | 发给选定 endpoint 的模型名称 |

`ResponseOrigin` 同时保留请求 endpoint、protocol、请求 model、provider 名称和 provider 实际报告的 model。错误和 Debug 输出不包含 secret。

## Endpoint 与持久配置

`EndpointConfig` 是可持久化、非 secret 的 wire 配置：

- `ChatGptSubscription`；
- `OpenAiResponses { base_url }`；
- `OpenAiChatCompletions { base_url }`；
- `AnthropicMessages { base_url }`。

`None` base URL 使用官方 endpoint；compatible URL 必须是无嵌入凭据、无 query 的绝对 HTTP(S) URL。App 与 Adapters 使用同一条校验规则，HTTP 可用于本地和受控兼容服务。

公开 endpoint constructors 是：

- `openai_responses::official` / `compatible`；
- `openai_chat_completions::official` / `compatible`；
- `anthropic_messages::official` / `compatible`；
- `chatgpt_subscription::connect` / `connect_cached`。

`ModelOptions` 是模型级、协议特定控制的唯一表示，同时用于持久配置和 `Request`。当前只有 OpenAI Responses reasoning 控制：`ConfiguredModel::new` 在构造时校验它与 `model.protocol()` 匹配，`Request` 在发网前再拒绝空的 Responses options，之后每次请求只应用已经验证的值。不会维护一套持久 options、一套请求 options 和手写 JSON 之间的重复转换，也不会把不支持的字段静默丢给其他协议。

API key 和 OAuth cache 都不属于 `EndpointConfig` 或 `ModelOptions`。App 在 composition root 取得凭据后才构造 live Endpoint。

## Request、上下文与身份

`Request` 只包含 BONE 真正会发的东西：

- 有序的输入文本（`InputItem::external`，其来源区分用户输入与具名上下文块）；
- 独立的高权限 instructions；
- tool definitions 和 `require_tool`（模型必须调用的那一个）；
- 可选、与 Model protocol 一致的 `ModelOptions`。

`InputItem` 只有一种形态：带来源归属的文本。上下文来源不会仅靠文本标签伪造角色，也不存在 assistant example、assistant replay 或工具结果 item。

没有多轮 replay 面：生产路径每一轮上下文都由 kernel 重新构造（见 [Core](core.md) 的 job 树与 context 组装），从不把 provider 的 assistant message、reasoning signature 或 tool result 回放给模型。既然回放不存在，协议层对应的那几项能力——响应转成下一轮输入、工具结果 item、跨轮核对 provider 工具身份——也一并删除，而不是留一层没有调用方的兼容面。

Request 在网络 I/O 前验证：

- input、instructions、tool name / description 和 schema 合法；
- `require_tool` 指向已经声明的工具；
- `ModelOptions` 非空且与 endpoint protocol 一致。

一个调用方传入的选项要么被明确发送，要么在网络前返回 typed error，不会被静默丢弃。没有调用方设置的选项不存在——包括输出 token 上限和 JSON schema 输出：需要时连同需要它的调用路径一起加回来，而不是先留着一个空档。

Anthropic 的 `/v1/messages` 要求每个请求都带 `max_tokens`，而 BONE 不发送自己的上限，因此这个值全部来自 Rig 的 per-model 默认表（`claude-sonnet-4-6` → 64000）。BONE 不自己编造这个数字；代价是 Rig 表里没有的 model id（例如某些 Anthropic-compatible 网关自定的名字）会以 `max_tokens must be set for Anthropic` 明确失败，而不是被一个猜测的上限静默截断。

## Response 与 streaming

streaming 是唯一的模型调用模式，terminal `Response` 是模型输出的唯一可信记录。它包含有序 `OutputItem`、usage、finish reason、响应 / 请求 ID 和来源身份；`text()` 只是汇总文本的便利方法，完整工具调用仍从 terminal Response 读取。

`ResponseStream` 可以发出文本、reasoning 和工具参数 delta 供界面显示，但成功流必须满足：

1. provider 发出真实 terminal；
2. adapter 完整 drain 并聚合响应；
3. 恰好发出一个 `StreamEvent::Completed(Response)`；
4. 随后结束。

delta 只服务显示，consumer 只拿最终结果时可以忽略它们。EOF 而没有 terminal 是 `IncompleteStream`；截断、过滤或反序列化失败不会把 partial output 提交成成功响应。第一个 provider stream error 立即成为终态，adapter 不继续等待一个已经失败的连接。完整工具调用只有 terminal `Response` 这一处可信来源。

`ErrorKind` 区分配置、请求、传输、provider、协议、stream 不完整等稳定类别；provider request ID 与经过大小约束的错误正文会保留用于诊断，secret 不进入错误。

## Core 的 ModelAdapter

`ModelAdapter` 持有两个已连接并验证过的 `ConfiguredModel`：

- 会话模型执行 `model_contract::converse`；
- Worker 执行 `model_contract::work` 和 `compact`。

每个 Core port 方法只进行一次 provider 调用。Adapter 把 Core 提供的 instructions、context JSON 和提交 schema 构造成一次强制 specific-tool Request，并要求模型返回恰好一个正确命名的提交调用。截断输出、零个或多个提交、错误工具名以及 schema decode 失败都转换为 `CallError`。

Core Runtime 在 Future 外层掌握 timeout 和提交资格。ModelAdapter 在调用前尊重已经到达的取消信号，但不宣称能停止 provider 已经开始的远端计算。模型调用走 streaming：文本和 reasoning delta 合并成短进度记录，通过调用已有的 progress channel 上报给界面，而 Core 只在完整结构化提案结束后提交状态，所以显示层的失败不会改变决策。

## 原生工具

`ToolEnvironment` 捕获 canonical workspace root 和经过验证的 `ToolLimits`。每个工具实现统一的 `Tool` trait，参数与输出是 typed serde values，model schema 由实现本身给出。

| 工具 | 行为 |
| --- | --- |
| `read` | UTF-8 文件分页，正文保留源文本，行号由界面展示；同时限制文件、行数和输出字节 |
| `glob` | workspace walker 上的 glob；尊重有界的本地 ignore 文件，限制遍历和结果 |
| `grep` | 基于 ripgrep Rust libraries 的 regex / literal 搜索；支持 glob、上下文、binary 检测和有界结果 |
| `apply_patch` | Codex patch grammar 的 Add / Update / Delete / Move；全量解析与 staging 后再提交 |
| `bash` | 一次非交互 `bash -c`；限制 cwd、deadline、stdout / stderr，并清理 Unix process group |

`read_only_tools` 把 read、glob、grep 注册成 Core `ToolPort`。App 添加 `session_history`，并用带 durable write tracking 的 adapter 注册 apply_patch 与 bash。工具目录始终完整；每个 Job 能调用哪些工具由 Core 的固定授权集合决定。工具 effect 由代码注册时决定，不能由模型 arguments 改写。Adapter 不维护第二套权限规则。

工具调用需要活跃 Tokio runtime。Core 负责调用级 timeout、并发、soft progress 和 cooperative cancellation；具体工具仍可有更窄的领域 deadline。参数只能缩小 hard limits，不能扩大。

## 文件与搜索边界

read、glob 和 grep 接受 workspace-relative path，或已经位于 workspace 内的绝对 path。普通 `..` 逃逸和 symlink 逃逸会被拒绝。这个检查是产品边界，不是 capability filesystem；运行不可信代码仍需宿主提供 OS sandbox。

目录搜索的规则：

- 读取搜索 root 及其以下有界的 `.ignore`、`.gitignore` 和安全的 `.git/info/exclude`；
- 不读取 root 以上的 parent ignore 或用户 / global Git excludes；
- 永不进入 VCS metadata；
- symlink 和不安全 ignore source 对该目录 fail closed，并返回 warning；
- hidden 和 ignored entry 也计入遍历上限；
- retained results 最后排序，但发生遍历 / 结果截断时，所选子集可能受文件系统枚举顺序影响。

grep 对单文件大小、总搜索字节、regex 大小、match 数、上下文和单行输出分别设限，并跳过检测到的 binary 文件。

## Patch 事务语义

Patch path 必须是相对路径，不得包含 `..`，也不得穿过已有 symlink；Add 与 Move 不覆盖已有目标。

在第一次目标文件变更前，apply_patch 完成：

- grammar 解析、路径解析和冲突检查；
- 所有 source snapshot 与 context matching；
- concurrent-change validation；
- replacement temporary files 与 rollback backup。

内部每个 `StagedAction` variant 自己携带该动作提交和回滚所需的路径、snapshot、replacement 或 backup，不使用两个平行数组再依赖位置配对，因此计划与暂存不可能产生类型错配。

提交尽可能使用 same-directory atomic replacement。后续 action 失败时，工具按相反顺序回滚已经提交的 action。它不是文件系统事务：创建过的空 parent directory 可能留下，极端 rollback failure 可能留下变更；错误会报告受影响的 workspace-relative path，并在可能时保留 recovery copy。

调用 Future 被取消时，App adapter 让已进入提交阶段的 transaction 脱离调用等待并完成提交或 rollback。进程崩溃、机器掉电和 hostile concurrent path replacement 超出这一内存保证。

## Bash 进程边界

Bash 只保证初始 `cwd` 位于 workspace；shell 仍可访问宿主其他路径、进程和网络。没有基于 command string 的 denylist。

默认 child environment 先清空，再复制常用 PATH、locale、terminal、temporary directory 和基础 Windows runtime 变量。`HOME`、`BASH_ENV`、proxy、cloud / provider credential 和 SSH agent 变量不会继承。宿主可以提供完整 replacement environment，但不应在其中放 secret。

Bash 分别限制 stdout 和 stderr，非零退出是结构化工具失败。Unix 上使用独立 process group，deadline 或取消后终止整组并等待清理。`ToolLimits::default_bash_timeout` 不得超过 `max_bash_timeout`；App 开启写工具时还要求 Core `tool_timeout` 更大，以给清理与结果持久化留出时间。

## ChatGPT subscription 与 Rig 边界

ChatGPT subscription connector 接受 App 验证过的 private `auth.json` path。构造 cached Endpoint 不读取凭据；每次模型请求由 Rig 在需要时读取或刷新 token。Rig 独占 JSON schema、OAuth 网络协议和跨进程 cache 事务，Adapters 不搜索 credential root，也不读取 OAuth bytes。

refresh、rejected-token invalidate 和 logout 在同一把文件锁下重新读取并原子提交，因此并发进程共享 cache 时不会形成 refresh storm，也不会用旧 401 删除新 token。设备授权等待在锁外进行。subscription backend 不支持的请求选项 BONE 根本不发送，因此没有需要本地拒绝的兼容判断。

Workspace 固定使用本地 [Rig 0.42.0 hardening patch](../patches/rig-core-0.42.0-chatgpt-hardening.md)。升级 Rig 时必须重跑独立 patch tests 和 BONE 的 provider contracts，确认上游已覆盖补丁退出条件后才能删除。

## 代码入口

- [`llm/mod.rs`](../crates/bone-adapters/src/llm/mod.rs)：公开模型协议 API。
- [`llm/protocol/`](../crates/bone-adapters/src/llm/protocol/)：三种 wire protocol。
- [`llm/service/`](../crates/bone-adapters/src/llm/service/)：ChatGPT subscription 服务边界。
- [`agent/model.rs`](../crates/bone-adapters/src/agent/model.rs)：Core 结构化模型契约适配。
- [`tools/`](../crates/bone-adapters/src/tools/)：工具类型、workspace path 和实现。

产品配置、凭据和写追踪见 [App](app.md)，Kernel 授权与调用生命周期见 [Core](core.md)，离线 wire contracts 与 live probes 见 [Testing](testing.md)。
