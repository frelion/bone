# bone-core

`bone-core` 是 BONE 的可信 Agent 内核和异步 Runtime。Rust Kernel 是唯一状态写入者；模型只能提交类型化提案；Runtime 只执行 Kernel 已批准的单次模型、工具和计时调用。

```text
host ──► Agent ──► Runtime actor ──► ModelPort / ToolPort
           ▲              │
           └── Observation ◄── Kernel::step(Event)
```

Core 不读取配置、凭据、网络、文件系统或进程环境。具体 provider 与工具由 `bone-adapters` 实现，持久化和产品生命周期由 `bone-app` 负责。

## 入口

```rust,ignore
use std::sync::Arc;
use bone_core::{Agent, AgentLimits, Input, InputId, ModelPort, ToolPort};

let model: Arc<dyn ModelPort> = configured_model;
let tools: Vec<Arc<dyn ToolPort>> = configured_tools;
let agent = Agent::with_ports(model, tools, AgentLimits::default())?;
agent.post(Input::new(InputId(1), "inspect the workspace")).await?;
let observation = agent.observe().await?;
```

可执行的确定性示例：

```sh
cargo run -p bone-core --example walkthrough --locked
```

## 核心约束

- Job 由 `Spec`、局部 `Context`、可替换 `Report` 和唯一终态 `Outcome` 组成；父子拥有关系决定交付和取消传播。
- 协调模型组织 root Job，Worker 推进自己的 Job。ID、版本、权限、提交资格和终态都由代码裁决。
- 权威 Record 正文只保存一次；每个 Job 只持有获准读取的序号和 checkpoint。跨 Job 只开放显式发布的报告、结果和 evidence。
- 每个 Job 同时最多有一个有效 Worker。等待不占模型槽；交互 root 有保留槽；模型超时、暂停、重配置和新约束会撤销旧提案资格。
- 迟到工具结果仍是真实事实。`ExternalEffect::Unknown` 必须由宿主核查并通过 `resolve_write` 确认，不能自动重试。
- `observe` 返回原子 baseline 与其后的 Record receiver；`shutdown` 返回冻结终态和仍未解决的写入。

完整设计见 [Core architecture](../../docs/core.md)。应用装配见 [App architecture](../../docs/app.md)，测试约定见 [Testing](../../docs/testing.md)。公开 API 的精确字段与签名以本 crate rustdoc 为准。
