# BONE

BONE 是一个用 Rust 编写的 coding agent。一个内核管理持续会话与执行 Job：Kernel model 负责理解和回应用户，Worker model 在固定工具权限内执行任务。当前 workspace 包含可信执行内核、基础设施适配器、headless 应用层和终端界面。

```text
future desktop / web / automation
                    │
                    ▼
                bone-tui
                    │
                    ▼
                bone-app
               ╱        ╲
              ▼          ▼
       bone-adapters ──► bone-core
```

- [`bone-core`](crates/bone-core/) 是唯一的 Agent 状态机：解释输入、组织 Job、构造局部上下文、授权模型与工具调用，并裁决取消、完成和迟到结果。
- [`bone-adapters`](crates/bone-adapters/) 实现 LLM 协议、模型端口和 workspace 内的读、搜索、补丁与命令工具。它依赖 Core 的端口，Core 不依赖任何 provider、文件系统或进程实现。
- [`bone-app`](crates/bone-app/) 是 composition root：管理 Workspace、Session、配置、凭据、SQLite 持久化、外部写事实和 Runtime 生命周期，并向所有前端提供同一套 Rust API。
- [`bone-tui`](crates/bone-tui/) 提供可直接运行的 `bone` 终端程序。

原来的 `bone-store` 已成为 `bone-app` 的私有模块。新的 TUI 是独立前端，只依赖 `bone-app`。

## 下载和运行

从 [GitHub Releases](https://github.com/frelion/bone/releases/latest) 下载与你的系统匹配的文件：

| 系统 | 文件 |
| --- | --- |
| Linux x86-64 | `bone-linux-x86_64` |
| Linux ARM64 | `bone-linux-aarch64` |
| macOS Intel | `bone-macos-x86_64` |
| macOS Apple Silicon | `bone-macos-aarch64` |
| Windows x86-64 | `bone-windows-x86_64.exe` |

Linux 和 macOS 下载后赋予执行权限即可运行：

```sh
chmod +x ./bone-*
./bone-linux-x86_64 # 按实际下载的文件名替换
```

Windows 可直接运行下载的 `.exe`。`SHA256SUMS` 可用于校验下载文件。
两个 Linux 发布文件均以 musl 静态链接，不依赖目标系统的 glibc、DBus 或 Secret Service。

### 在项目里执行一次任务

交互使用直接运行 `bone`。自动化、真实场景测试或 CI 使用同一个二进制的
`bone run`，它不启动 TUI，完成后在 stdout 输出 JSON：

```sh
# 先运行 bone，在 Models & connections 中保存 API key
bone run \
  --workspace /path/to/project \
  --prompt '修复失败的测试，并运行相关测试验证修改' \
  --model gpt-5.6-sol \
  --trajectory ./artifacts/bone-trajectory.json \
  --result ./artifacts/bone-result.json
```

也可以用 `--prompt-file issue.md` 从文件读取任务。交互界面和 headless 使用相同的
Job 工具授权：普通任务默认可使用全部已注册工具；只做调查时，在请求中说明
“只读分析，不要修改”。会话模型提出限制，Core 创建权限固定的 Job，并在每次工具
调用前校验授权。`bone run` 不读取 API-key 环境变量；API key
必须已经由 TUI 保存，或通过 stdin 交给 `bone credentials set`，最终写入
`$BONE_HOME/credentials.toml`（默认 `~/.bone/credentials.toml`）。
运行 `bone run --help` 可查看 provider、自定义 HTTPS endpoint、项目配置授权、超时和
退出码参数。

权威 benchmark 的 Harbor 适配、公开测试集选择和结果留存规范见
[`benchmarks/README.md`](benchmarks/README.md)。

## 最小使用路径

`App` 打开一份宿主指定的数据目录；Workspace 精确绑定到一个已经存在的目录；Session 是可持久恢复的用户工作单元。

```rust,no_run
use bone_app::{App, AppOptions, SessionSeq, SubmitInput};

# async fn run() -> bone_app::Result<()> {
let app = App::open(AppOptions::with_paths(
    "/absolute/path/to/app-data",
    "/absolute/path/to/bone-home",
)).await?;
let workspace = app.open_workspace("/absolute/path/to/workspace").await?;
let session = app.create_session(workspace.id, "Investigate build").await?;

let mut changes = session.observe();
let receipt = session.submit(SubmitInput::new("Find the failing test")).await?;
let page = session.history(SessionSeq(0), 100).await?;

# let _ = (&mut changes, receipt, page);
app.shutdown().await?;
# Ok(())
# }
```

提交成功表示输入及其幂等键已经持久化，不表示 Agent 已经执行完成。未选模型、缺少凭据或 provider 暂时不可用时，输入保留在 Session 中等待恢复。当前状态通过 `Session::observe` / `snapshot` 获取，耐久历史通过 `Session::history` 按游标读取。

Runtime 配置按以下顺序解析：

```text
Session override > Project override > User setting
```

`App::update_config` 校验并保存一项类型化变更，然后把最新 desired 配置通知给受影响的已打开 Session；它不等待模型连接或网络认证。面向前端的 `ConfigChange::Model` 会在一个事务中同时选择 worker/coordinator。运行中的 Session 异步装配新端口，期间保留 Runtime ID、Job 图和在途工具；旧模型调用失去提交资格并在新模型上重新调度，已经开始的工具继续收尾。装配失败时 Session 保持暂停并暴露可匹配的问题，直到配置或凭据修复。

## 持久化边界

`AppOptions` 分别携带 SQLite `data_dir` 与文件配置根目录 `bone_home`。平台默认的
`bone_home` 是 `BONE_HOME` 指定的绝对目录，否则为 `~/.bone`。User 配置与 Profiles
位于 `config.toml`，API key 位于权限受限的 `credentials.toml`，ChatGPT OAuth cache
位于 `providers/`；SQLite 只保留 Session override、项目配置的摘要信任记录和其他运行
状态。旧 SQLite User/Workspace 配置与系统 Keyring 不迁移，升级后需重新配置连接。

一次外部写在调用前记录意图，在结果被匹配的 Agent 事实确认前保持阻塞。进程退出后，App 恢复产品状态并把丢失 Runtime 的未完成输入标为 `Interrupted`，不会猜测或自动重放结果未知的外部写。`App::unresolved_writes` 是 Workspace 级的权威查询入口。

## 文档

长期维护的设计文档只有以下六个入口：

- [Core](docs/core.md)：Job、Context、调度、权限、并发和模型行为契约。
- [App](docs/app.md)：Workspace / Session API、配置、持久化、恢复和外部写。
- [Adapters](docs/adapters.md)：LLM 协议、模型适配器、内置工具和安全边界。
- [Testing](docs/testing.md)：测试分层、替身规范、执行矩阵和 live certification。
- [TUI](docs/tui.md)：下一阶段前端的产品边界、状态流和验收范围。
- 本文件：项目定位、依赖方向和开发入口。

公开 Rust 类型、字段和方法以 crate rustdoc 为准；上述文档只维护跨模块不变量和设计取舍。固定的 Rig 上游补丁边界记录在 [`patches/`](patches/) 中。

## 验证

普通变更应至少通过：

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-targets --all-features --locked
cargo test --workspace --doc --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked
```

`--all-features` 会启用只用于离线协议契约的 `test-utils`。真实 provider 测试是显式、可能收费的独立入口，详见 [Testing](docs/testing.md)。

[`legacy/`](legacy/) 只保存历史材料，不属于 workspace build；[`third_party/`](third_party/) 保存固定版本的上游源码与本地补丁。
