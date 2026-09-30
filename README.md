# BONE

BONE 是一个 Rust 编写的 coding agent。用户与一个 agent 对话；agent 在内部 Job 中保留正在进行的工作、等待关系和局部上下文。用户直接提出任务和后续指令，无需创建或选择 Job。

这是从零重写的单 crate 实现。模型连接、消息、工具定义、返回结果和流解析使用官方 Rig SDK，固定到 Git 提交 `063bcf0e9cee2fd5287fbb807e67d3e9d418ba0a`。会话状态与事件存于 SQLite。架构边界见 [architecture.md](docs/architecture.md)，验收和消融方法见 [verification.md](docs/verification.md)。

## 构建与配置

需要 Rust 1.96 或更新版本；本地协议验收还需要 Python 3。

```sh
cargo build --locked
cargo install --path . --locked
bone providers
```

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

BONE 独立登录的订阅缓存仅存于当前数据目录的 `profiles/PROFILE/auth.json`。每次调用重新通过 Rig 读取或刷新缓存，并持有该缓存的跨进程锁直到该调用结束。复用 Codex 登录时，锁由认证文件的 canonical 路径标识，保存在固定 `~/.bone/v2/credential-locks`，因此不同数据目录和 profile 仍共享同一来源的调用锁。等待锁可随调用取消。普通运行不会启动交互登录。独立缓存失效或 HTTP 401 会报告重新登录；显式 `login` 成功后才替换已有缓存。开启复用时，`bone login` 只检查现有 Codex 登录资料是否可加载，不发起登录或联网验证。

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

默认每个根用户输入与它引起的内部工作共享 64 次模型调用和 16 个新 Job 的预算，最多同时执行 3 个动作。可以通过 `--max-calls`、`--max-jobs`、`--max-parallel`、`--context-chars`、`--model-timeout-seconds` 和 `--timeout-seconds` 控制运行。`--single-job` 与 `--no-compaction` 用于消融。上下文摘要压缩预算内已完成的安全前缀，保留未处理输入的原文，完整工具 batch 不会被截断。

## 工具权限与中断写入

`--read-only` 禁用 `write_file` 与 `shell`。文件工具限制在 workspace 内，拒绝路径向上遍历和已知 symlink escape。`write_file` 必须携带读取结果的 SHA-256；创建新文件使用 `expected_sha256 = null`。

`shell` 在 workspace 中以当前本地用户权限运行，没有 OS sandbox。命令超时或取消时会终止所启动的进程组；输出最多保留每个流 32 KiB。不要把进程组终止等同于撤销已发生的外部效果。

中断或无法确认完成的写操作记录为未知写；进一步写入需要先核查。文件替换后目录同步失败也属于未知效果。查看 session history 与实际文件/外部状态，再记录观察：

```sh
bone reconcile SESSION_ID CALL_ID --note '核查后的实际结果和证据'
bone --profile work resume SESSION_ID
```

`reconcile` 记录检查结果并解除写入阻塞，不自动重放旧命令。恢复会话可以继续读取和核查；未知效果不能据此报告为成功。

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

最终离线验收通过 65 项 Rust 测试、5 项 Python 测试、Clippy 和全部 feature 编译。复用现有 Codex 登录、使用 `chatgpt:gpt-6-luna` 的中文连续对话 5/5 通过，覆盖解释、修改、继续、停止与恢复。正式消融 18 次中 15 次通过；三个失败均为关闭压缩后的上下文超限。小任务多 Job 开销更高，不能据此宣称普遍效率收益。

一次真实 API-key 验证收到 HTTP 429 `credit_balance_exhausted`，没有成功的模型响应，也没有盲目重试。其他 provider 的真实连接、云 companion 请求和 Candle artifact 推理尚未实测。完整实验、保留的失败记录和验证边界见 [verification.md](docs/verification.md)。
