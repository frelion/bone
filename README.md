# BONE

BONE 是一个 Rust 编写的 coding agent。用户与一个 agent 对话；agent 在内部 Job 中保留正在进行的工作、等待关系和局部上下文。用户直接提出任务和后续指令，无需创建或选择 Job。

这是从零重写的单 crate 实现。模型连接、消息、工具定义、返回结果和流解析使用官方 Rig SDK，固定到 Git 提交 `063bcf0e9cee2fd5287fbb807e67d3e9d418ba0a`。会话状态与事件存于 SQLite。架构边界见 [architecture.md](docs/architecture.md)，验收和消融方法见 [verification.md](docs/verification.md)。

测试入口与覆盖边界见 [测试地图](tests/README.md)，本次实际删减与复验见 [收缩记录](docs/results/2026-10-02-contraction/README.md)。新版 TUI 的交互、逐屏过程与验证范围见 [TUI 设计交付](docs/results/2026-10-02-tui-design/README.md)。同一内核使用 `gpt-6-luna` 完成了跨进程、多轮修改与冷历史回查的真实仓库任务，并在外部反馈修正后通过功能验收和实际工具演示。原先失败、人工介入及验证边界保留在[工程交付记录](docs/results/2026-10-01-flagship-engineering/README.md)中；这不是通用任务成功率保证。

## 查看可用工具

```sh
bone tools
bone tools --json
bone tools --read-only --single-job --json
```

该命令只输出运行时使用的 Rig 原生 `ToolDefinition` 元数据；JSON 为定义数组，文本为名称与描述。它不读取配置或认证、不连接模型、不创建会话，也不执行文件工具。`--read-only` 保留 `search_files`，移除 `edit_file`、`write_file` 和 `shell`；`--single-job` 移除跨 Job 控制工具，保留当前 Job 的普通工具。传入全局 profile/model 选项不会触发模型配置。

普通搜索和编辑仍须经过 `run/chat/tui → Engine → Job → tool`，元数据入口不公开执行器。工具用法、实现来源与验证边界见 [工程工具](docs/engineering-tools.md)。

## 构建与配置

需要 Rust 1.96 或更新版本；本地协议验收还需要 Python 3。

```sh
cargo build --locked
cargo install --path . --locked --force
bone --version
bone providers
```

安装后的版本应为 `bone 0.9.0`。如果仍显示旧版，用 `command -v bone` 检查实际入口；其他目录中更靠前的旧 launcher 会遮住 `~/.cargo/bin/bone`。可以先用 `~/.cargo/bin/bone tui` 启动，或将原 launcher 备份后指向这个安装位置。

`target/release/bone` 是构建目录内的文件，只有在包含它的 checkout 中才能通过相对路径运行。安装后的 `bone` 可在任意项目目录中使用。

`bone providers` 从 Rig 的注册表列出全部 provider/protocol 组合，并加入 Cohere、Ollama 和本次构建启用的官方 companion provider。某些 provider 支持多种协议，使用列出的限定名称，例如 `moonshot/openai:MODEL`。

默认数据目录是 `~/.bone/v2`。`--data-dir DIR` 或 `BONE_DATA_DIR` 可指定独立目录。生成基础配置：

```sh
mkdir -p ~/.bone/v2
bone config > ~/.bone/v2/config.toml
```

配置中的模型引用和 provider 配置由 Rig 负责解析；API key 用环境变量提供。以下配置引用变量名称，不保存 key 值：

```toml
default_profile = "work"

[profiles.work]
model = "openai/openai:gpt-5.4"
credential_env = "OPENAI_API_KEY"
max_tokens = 4096

[profiles.subscription]
model = "chatgpt/openai:gpt-6-luna"
reuse_codex_login = true
```

未设置 `credential_env` 时，普通注册 provider 使用 Rig 原生环境变量规则，包括 endpoint 和备用凭据。显式 endpoint 使用原生 `ProviderRef` 的配置形式，例如：

```toml
[profiles.gateway]
credential_env = "OPENAI_API_KEY"

[profiles.gateway.model]
model = "YOUR_MODEL_ID"

[profiles.gateway.model.config.openai]
api_key = "[redacted]"
base_url = "https://YOUR_GATEWAY/v1"
dialect = "openai"
auth = "Bearer"
route = "Responses"
```

这里的 `api_key` 是 Rig 序列化的占位符，运行时从 `credential_env` 注入真实值。`additional_params` 是原生 `CompletionRequest` 的额外参数对象；例如 `[profiles.work.additional_params.reasoning]` 下的 `effort = "low"`，具体参数需被所选模型支持。

`max_tokens` 也遵循 Rig 的原生 provider 行为。本次固定版本的 ChatGPT 订阅接入不向服务端发送 `max_output_tokens`，因此该路径不能用它保证输出 token 上限；BONE 的调用额度、上下文大小与运行时限仍独立生效。

命令行可直接选择原生引用；已有配置时，`--model` 只覆盖所选（或默认）profile 的模型，保留凭据来源、额外参数和输出上限，并验证它们与新 provider 的兼容性。没有配置时创建命令行临时 recipe。`BONE_MODEL` 也可提供此引用。`.env.local` 不会自动加载。

```sh
bone --model openai:gpt-5.4 run '说明这个项目的测试入口' --read-only
bone --model ollama:qwen3 chat
```

Ollama 的上述命令连接本机默认 daemon。Cohere 使用 `cohere:MODEL` 和 `COHERE_API_KEY`。自定义 Cohere/Ollama endpoint 可在 profile 的 `model` 对象中使用各自原生配置，如 `ollama = { base_url = "http://localhost:11434", api_key = "[redacted]" }` 以及 `model = "qwen3"`。

## API key 与订阅登录

通过当前 shell 或凭据管理工具注入所需的 API key 环境变量，然后运行命令。BONE 不将 API key 写入 profile 配置。

已有 Codex subscription 登录可直接复用。上面的 `subscription` profile 在每次请求前只读 `${CODEX_HOME:-~/.codex}/auth.json` 的当前 access token 和 account ID，交给 Rig 公开的 `AuthSource::AccessToken`。它不转换、复制、刷新或写入 Codex 的认证文件；认证过期时先刷新 Codex 登录，再运行 BONE。

```sh
bone --profile subscription chat
```

若需要 BONE 自己管理独立订阅缓存，设 `reuse_codex_login = false`，再使用 Rig 原生 device flow：

```sh
bone --profile subscription login
bone --profile subscription chat
```

也可以选择一个尚未创建 `config.toml` 的独立数据目录，使用命令行 profile：

```sh
bone --data-dir ~/.bone-personal --profile personal --model chatgpt:gpt-6-luna login
bone --data-dir ~/.bone-personal --profile personal --model chatgpt:gpt-6-luna chat
```

BONE 独立登录的订阅缓存仅存于当前数据目录的 `profiles/PROFILE/auth.json`。每次调用重新通过 Rig 读取或刷新缓存，并持有该缓存的跨进程锁直到该调用结束。复用 Codex 登录时，锁由认证文件的 canonical 路径标识，保存在固定 `~/.bone/v2/credential-locks`，因此不同数据目录和 profile 仍共享同一来源的调用锁。等待锁可随调用取消，不消耗单次模型请求的超时；CLI 总运行时限仍包含排队时间。普通运行不会启动交互登录。独立缓存失效或 HTTP 401 会报告重新登录；显式 `login` 成功后才替换已有缓存。开启复用时，`bone login` 只检查现有 Codex 登录资料是否可加载，不发起登录或联网验证。

## 终端界面

```sh
bone
bone tui
cargo run --locked -- tui
bone --profile subscription tui --workspace /path/to/project
bone --profile subscription tui --session SESSION_ID
```

TUI 左侧切换会话，右侧显示对话、Markdown 答复和工具行动；当前连接与模型固定可见。`/connect` 统一选择现有连接、ChatGPT 登录或 API，`/model` 自由修改原生模型名；选择保存为下次启动的默认配置。Job 由 Agent 自动管理。原生 Rig 流实时显示为“输出中（未交付）”；新要求、暂停或调用结束会清除旧预览，正式交付以 SQLite 记录为准。

默认正文不显示“你 / Agent”标题，原话以细竖线区分。输入框显示编辑焦点和真实终端光标。运行行固定显示实际动作、耗时与活动标记；输入回执独立显示，后台仍在执行时不会被较早的交付覆盖。

0.9.0 使用 Shift+方向键切换焦点。左栏一项一行，当前会话与浏览候选分开；表单按内容收紧，保存前回顾公开配置，密钥遮罩。文本选区使用 Ctrl+Shift+方向键，点击输入框恢复编辑。

本轮视觉对抗、架构减法与完整交互证据见 [0.9.0](docs/results/2026-10-03-product-refactor/overview.html)；此前左栏见 [0.8.1](docs/results/2026-10-03-tui-sidebar/overview.html)；此前会话栏、连接管理与终端证据见 [0.8.0 工作区交互](docs/results/2026-10-03-tui-workspace/README.md)；此前反馈修复见 [0.7.1](docs/results/2026-10-02-tui-feedback/README.md)。

| 操作 | 按键 / 命令 |
| --- | --- |
| 发送 / 换行 | Enter / Shift+Enter、Alt+Enter、Ctrl+J |
| 命令面板 / 文件引用 | Ctrl+P / `@文件` 后 Tab，或 Ctrl+O |
| 输入历史 / 编辑 / 撤销 | Alt+↑↓；Ctrl+A/E/K/W；Ctrl+Z / Alt+Z |
| 会话 / 原主焦点 / 正文 / 输入 | Shift+← / Shift+→ / Shift+↑ / Shift+↓ |
| 外部编辑器 | Ctrl+G，使用 VISUAL / EDITOR，返回草稿后 Enter 才发送 |
| 搜索对话 / 完整结果 / 审计 | Ctrl+F / 正文 d / 详情 D；F2 查看活动 |
| 暂停 / 恢复 / 退出 | Ctrl+C / Ctrl+R / Ctrl+Q |
| 新会话 / 模型 / 连接 | `/new` / `/model` / `/connect` |
| 项目修改 / 复制 / 帮助 | Ctrl+D / Ctrl+Y / F1 或 `/help` |

所有回复目标的未发送草稿、光标、选区及最近输入原子保存在数据目录的 `tui/` 下，切换和重启后恢复；撤销历史不跨进程保存。多行粘贴、文件补全、恢复草稿和外部编辑器均不自动提交。切换会话保存并暂停旧工作，打开的未完成工作保持暂停。连接和模型保存到配置；API key 单独存放且绑定 endpoint，聊天记录不保存它。设置不做 Job 外的模型探活；首次实际请求验证认证。

界面支持中文/emoji/组合字符、鼠标滚动、Markdown/code/diff 和 NO_COLOR。F2 按需打开全宽内部事件面板。启动按页读取最近 40 条记录，上翻至顶部自动读取更早记录；完整历史留在 SQLite。最近预览有明确数量和字符上限。模型预览允许丢帧，不承担持久化或交付职责；工具运行时显示 stdout/stderr 短尾部，完成后以持久原文为准。普通成功工具收成一行，失败先显示退出码与关键原因。

`/diff` 检查 staged/unstaged 修改及未跟踪文件实际内容；`/export` 异步导出本地 HTML 对话与行动日志，完成后 Ctrl+P 查看路径；`/copy` 使用系统剪贴板。用户的界面操作不发起 Agent 模型或工作区写工具。Agent 的行动始终由 Engine/Job 执行；未知写必须先核查，TUI `/reconcile` 记录结论后仍暂停；Ctrl+R 才继续。TUI 需要交互终端，管道输入使用 `bone chat`。

问题到达只提醒；选中问题按 Enter 或通过 Ctrl+P 明确选择回复；菜单“写新要求”返回原稿。Esc 返回上一层，保留目标与草稿；Ctrl+C 始终暂停，Ctrl+Y 独立复制。

详见 [终端界面设计与验收](docs/tui.md)。离线 PTY 验收：`python3 tests/tui_pty.py`，仅连接本地 fixture，不读取真实凭据。

## 使用会话

```sh
bone --profile work run '修复失败测试，并运行相关测试' --workspace /path/to/project --json
bone --profile work chat --workspace /path/to/project
bone sessions
bone history SESSION_ID --json
bone --profile work resume SESSION_ID --workspace /path/to/project --json
bone --profile work run '后续要求：仅修改 parser.rs' --session SESSION_ID
```

`run` 的 prompt 是位置参数。`chat` 中使用 `/stop`、`/resume`、`/quit`；Ctrl+C 暂停会话。会话 ID 出现在运行结果中，可用于恢复或查看记录。Job ID 是内部诊断信息；后续工作通常沿用同一个 session。

内部 Job 的提问会直接显示到会话，普通回复自动回答最近一个未答问题，无需选择 Job。多个问题同时等待时，其余问题保留在历史中。`run --json` 的等待结果包含 `question_id`，自动化可以用 `bone run '回答内容' --session SESSION_ID --reply-to QUESTION_ID --json` 回答指定问题。每条新回复有独立调用预算，原委托继续保留在会话中。

默认每个根用户输入与它引起的内部工作共享 64 次模型调用和 16 个新 Job 的预算，最多同时执行 3 个动作。可以通过 `--max-calls`、`--max-jobs`、`--max-parallel`、`--context-chars`、`--model-timeout-seconds` 和 `--timeout-seconds` 控制运行。`--single-job` 与 `--no-compaction` 用于消融。上下文摘要压缩预算内已完成的安全前缀，保留未处理输入的原文，不拆散工具调用与结果的配对。如果一个完整批次本身超过摘要预算，摘要请求使用带原文引用的有界结果预览。

摘要后的原文仍在 SQLite，Agent 可按事件 ID 分页回查。Idle/Closed Job 的历史正文按需加载；会话事件元信息仍随历史增长。`bone history` 是完整审计读取，可能占用较多内存。长会话可按事件 ID 有界续读：

```sh
bone history SESSION_ID --limit 100 --json
bone history SESSION_ID --after LAST_EVENT_ID --limit 100 --json
```

任一分页选项启用分页模式；默认页大小为 100，有效范围为 1～1000。分页 JSON 包含 `events`、`next_cursor` 和 `has_more`；将非空页的 `next_cursor` 作为下一次 `--after`。文本分页保留事件行并显示相同续读信息。分页按存储追加顺序读取原始事件（包括摘要压缩前的内容），未知 session 或不属于该 session 的游标会报错。Rust 调用方可使用 `bone::history_page(data_dir, session_id, after, limit)`；该只读审计 API 不需要模型配置或请求，不修改 session revision，也不取得执行 session 独占锁。

Rust 库的执行操作通过 `runtime::Engine`：`post`、`step`、`stop`、`resume`。`state()` 和 `options()` 只读；`read_event`、`events` 以及库级 `sessions`、`session`、`history` 供观察与审计。模型、工具和存储实现不直接公开，避免绕过 Job 的归属和恢复规则。

## 工具权限与中断写入

`--read-only` 禁用 `edit_file`、`write_file` 与 `shell`，保留 `search_files` 等读取工具。文件工具限制在 workspace 内，拒绝路径向上遍历和已知 symlink escape。`write_file` 必须携带读取结果的 SHA-256；创建新文件使用 `expected_sha256 = null`。

`read_file` 默认返回 8 KiB，按 `next_offset` 继续读取；显式 `limit` 最大为 32 KiB。刚返回的结果先交给工作模型；超限时压缩更早的已消费历史，再为新结果提供带原文引用的有界预览。Agent 可回查原事件补全省略部分。单条用户输入或原生调用参数本身无法装入上下文时仍会明确失败。

`shell` 在 workspace 中以当前本地用户权限运行，没有 OS sandbox。命令超时或取消时会终止所启动的进程组；输出最多保留每个流 32 KiB。不要把进程组终止等同于撤销已发生的外部效果。

命令默认超时为 60 秒；编译或集成测试可显式指定 `timeout_seconds`，有效范围 1～3600 秒。`run` / `resume` 的整体 `--timeout-seconds` 截止时间仍适用。无效超时值会报错；真正超时或取消后仍需核查未知写，不会自动重试。

`chat --timeout-seconds` 限制从最新普通输入或 `/resume` 开始的一段工作；等待用户时不计时。到期后持久暂停，可以继续同一对话。新输入重启时限，不重置旧请求的模型调用额度。

中断或无法确认完成的写操作记录为未知写；进一步写入需要先核查。文件替换后目录同步失败也属于未知效果。查看 session history 与实际文件/外部状态，再记录观察：

```sh
bone reconcile SESSION_ID CALL_ID --note '核查后的实际结果和证据'
bone --profile work resume SESSION_ID
```

`reconcile` 记录检查结果并解除写入阻塞，不自动重放旧命令。恢复会话可以继续读取和核查；未知效果不能据此报告为成功。

workspace 锁和未知写标记存放在操作系统用户目录的 `~/.bone/workspace-locks`，独立于数据目录和临时目录。同一系统用户、相同 canonical workspace 根共享写锁；嵌套但根不同的 workspace 不共享。前台 shell 继承实际锁；BONE 被硬杀后仍在运行的 shell 结束前，同根的其他会话不能写入或核销它的未知结果。该保证不覆盖主动关闭继承文件描述符、自行脱离的后台程序。已有文件的哈希复核也无法消除与任意外部编辑器之间的最后竞争窗口。

如果动作完成后的 SQLite 提交失败，执行器停止后续状态修改并要求重新打开，以持久记录恢复；未知写仍需核查。

## 官方 companion provider

```sh
cargo build --locked --features bedrock,vertexai,candle
```

- Bedrock：`--model bedrock:MODEL_ID`，使用 AWS SDK 的环境凭据与区域。
- Vertex AI：`--model vertexai:MODEL_ID`，使用官方 SDK 的 Application Default Credentials 与配置。
- Candle：在 profile 中设置本地 artifact 路径；Rig Candle 负责加载和原生 completion。

```toml
[profiles.local.model.candle]
config = "/absolute/path/config.json"
tokenizer = "/absolute/path/tokenizer.json"
weights = "/absolute/path/model.gguf"
gguf = true
```

云 provider 工厂和 Candle 工厂已通过 `cargo check --all-features`。构建成功不能证明账号可用、区域支持或 checkpoint 可推理；这些真实连接和 artifact 推理仍需对应环境验收。

## 验证与数据版本

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
cargo check --all-features --locked
python3 -B -m unittest discover -s tests -p 'test_ablate.py' -v
```

协议测试使用本地 Responses/SSE fixture、合成凭据和临时目录，验证原生模型调用、流收集、401 不重试、内部工作、工具和恢复。消融脚本默认只生成实验安排；只有显式 `--run` 才请求模型。离线测试不代表真实模型质量、效率提升或 subscription 登录成功。

新实现默认使用 `~/.bone/v2`，不自动导入旧会话或旧凭据。显式 `reuse_codex_login` 仅按用户选择读取现有 Codex 登录，不属于缓存迁移。旧实现保留在 Git 历史中。本次重写没有延续旧的多 crate/TUI 接口；当前用户入口是本仓库的 `bone` CLI。

长会话加固后的离线验收通过 97 项 Rust 检查（含 3 项公共接口编译失败检查）、5 项 Python 测试、Clippy 和全部 feature 编译。包括 12 轮续接、多次压缩、跨 Job 原始要求回查、当前与排队输入区分、纠正指令恢复、真实硬杀 shell、存储故障注入及稳定订阅锁。2,200 条大事件经四次压缩重开后，原文仍能回查。

两次真实 12 轮复杂任务的完整验收均保留失败：一次摘要遗漏具体待办，一次首轮网络失败后旧输入干扰当前任务、UTC 功能漏实现。修复后在原 Session 续接，已从冷历史查回具体要求；明确纠错后原 9 项功能检查全过。它验证了长会话恢复与纠错链路，尚不能作为无人干预任务成功率的保证。

2026-10-01 真实仓库验收中，BONE 使用 `gpt-6-luna` 自行实现了本项目的历史分页，覆盖存储、公共 API、CLI、测试和文档，并经历需求追加及暂停后跨进程续做。修复默认读取过大导致的首轮停滞后，重跑的首次交付通过独立检查 39/39；集成后 Rust 检查 102 项通过。首轮失败、28 次模型调用和 3 次摘要的重跑证据见 [工程验收记录](docs/results/2026-10-01-engineering/README.md)。这是一个真实需求的成功样本，尚未证明数小时或大型陌生仓库任务的稳定性。

此前重写阶段，复用现有 Codex 登录、使用 `chatgpt:gpt-6-luna` 的中文连续对话 5/5 通过，覆盖解释、修改、继续、停止与恢复。正式消融 18 次中 15 次通过；三个失败均为关闭压缩后的上下文超限。小任务多 Job 开销更高，不能据此宣称普遍效率收益。

一次真实 API-key 验证收到 HTTP 429 `credit_balance_exhausted`，没有成功的模型响应，也没有盲目重试。其他 provider 的真实连接、云 companion 请求和 Candle artifact 推理尚未实测。完整实验、保留的失败记录和验证边界见 [verification.md](docs/verification.md)。
