# bone-agent 场景验证

本轮只验证进程内 bone-agent；bone-app 不迁移。场景推演、受控执行测试和真实外部系统能力分别标注，不把未实现的能力写成“已通过”。

## 自动化验收入口

```sh
cargo fmt -p bone-agent --check
cargo clippy -p bone-agent --all-targets --all-features --locked -- -D warnings
cargo test -p bone-agent --all-targets --all-features --locked
cargo test -p bone-agent --doc --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo doc -p bone-agent --no-deps --all-features --locked
cargo run -p bone-agent --example walkthrough --locked
cargo run -p bone-agent --example interleaving --locked
```

Kernel 测试直接驱动真实状态机；Runtime 测试使用受控模型、假工具和 Tokio 暂停时钟；agent_slice 使用实际 ModelAdapter、受控 provider 和真实 read 工具。没有新增测试框架，没有使用真实账号执行外部写入。

2026-09-08 本轮结果：133 项测试全部通过，其中 Kernel 回归 69 项；格式检查、Clippy 零警告、Rustdoc 零警告和两个可执行示例均通过。文档测试命令通过，但当前没有 doctest。未验证 bone-app 或整个 workspace 的构建。

## 32 个场景的覆盖边界

| 场景 | 验证方式或首版边界 |
| --- | --- |
| S01 简单问答 | Kernel 短分派后完整主力回答，不经过额外审批。 |
| S02 A 重活中启动 B 重活 | Kernel 与主力并行；B 可以用工具，A 不取消。 |
| S03 多工作进度 | Job/Call 独立快照和进度；主力只看到公开状态，不推测隐藏推理。 |
| S04 大量等待与就绪工作 | 有界槽位、后台轮转及工具候选公平性；不为等待常驻模型。 |
| S05 单个模型失败 | 所属 Job 失败，不误伤别的工作；路由失败显式重试。 |
| S06 等待用户时来新消息 | AskUser 与关联补充；旧未决解释与新原话有序合批。 |
| S07 答案交付前更正要求 | 暂扣候选、撤销旧资格；首版不流式发布未提交答案。 |
| S08 B 完成，A/C 继续 | 局部 JobFinished，不取消其他工作或定时器。 |
| S09 修改已有目标 | 撤销旧 Call，迟到成功/失败/进度不能覆盖新版本。 |
| S10 一条输入改变多个 Job | 整组控制校验、父子冲突拒绝，无部分应用。 |
| S11 停止期间分派迟到 | Stop 撤销已接收批次；新输入可创建新工作。 |
| S12 未来工作共享要求 | constraints 进入后续模型上下文；不声称任意自然语言可硬性强制。 |
| S13 含糊目标 | Investigate/Clarify；调查有明确续接或失效，不默认为无影响。 |
| S14 暂停后恢复 | 用户输入顺序与版本；source 不能自行解除用户暂停。 |
| S15 材料影响相关工作 | 明确引用和等待；普通追加材料不全局作废，确证 Unknown 则重新考虑相关提案。 |
| S16 子工作与成果复用 | 取消归属、引用不取消、成果等待和终态等待、循环拒绝。 |
| S17 并发改同一文件 | 工具边界要求隔离或条件写；未接入写文件工具，不宣称已实现。 |
| S18 共用浏览器 | 明确延期，不制造假浏览器验收。 |
| S19 用户同时改外部文档 | 具体适配器的版本/ETag 前置条件；非当前内核提供的事务。 |
| S20 跨 Job 共享金额预算 | 未实现通用预算预留/核销；不把自然语言 constraints 当硬预算。 |
| S21 撤销权限或切换账号 | 配置不可漂移、Stop 与宿主权限边界；动态账号交接不在首版。 |
| S22 工具/子模型伪造授权 | 模型角色和工具结果不能互相冒充；source 不能修改目标/解除暂停/越出工作树。 |
| S23 多租户保密 | 首版单宿主会话边界；没有通用多租户隔离系统。 |
| S24 只读请求的数据出境 | 注册适配器和宿主负责；ReadOnly 不是无泄密证明。 |
| S25 先接收“不要发送” | 未决输入阻止新业务写的开始授权。 |
| S26 先授权发送再停止 | 受控写工具可拒绝取消并报告真实 Applied，不伪造撤回。 |
| S27 写结果 Unknown | 保留许可、禁止自动重发、宿主幂等确证与相关提案重算。 |
| S28 重复及迟到回调 | 不重复回复或执行，不覆盖替代工作。 |
| S29 进程内定时跟进 | 等待不占模型，唤醒只作用所属 Job，旧定时不复活停止工作。 |
| S30 重启后的次日跟进 | 明确延期；shutdown 报告未决调用，不自动重放。 |
| S31 输入/进度洪流 | 普通容量、澄清保留入口、Stop 独立入口、进度合并、Lagged 基线恢复。 |
| S32 20ms 物理急停 | 明确不支持硬实时；必须依赖独立确定性控制系统。 |

## 交叉审判转成回归

重点反例不是“架构看起来可行”，而是具体顺序下不允许发生的动作：

- 旧工作与新目标交错，旧成功、错误和进度都不能夺回提交权。
- 停止发生在 Kernel 模型返回前，不能从迟到分派创建工作。
- 较早解释失败后接收较新要求，旧批次不能经重试覆盖新要求。
- 调查已被取代、取消或源目标改变，迟到材料不能重开旧批次或误停新工作。
- 两个被扣住的候选在放行时形成互等，提交时必须拒绝循环。
- 工具容量为一时，低 ID 的快速循环不能饿死先等待的另一工作。
- Unknown 被确证为已执行后，旧重试候选不能自动变成第二次写入。
- 一个输入需要多项交付，一条 Reply 或单个 Job 结束不能提前结束请求。

测试分别位于 [kernel](../crates/bone-agent/tests/kernel.rs)、[runtime](../crates/bone-agent/tests/runtime.rs)、[interleavings](../crates/bone-agent/tests/interleavings.rs)、[模型并发](../crates/bone-agent/tests/model_concurrency.rs)和[完整切片](../crates/bone-agent/tests/agent_slice.rs)。配置、上下文投影、协议结构、取消和诊断脱敏另有单元测试。

没有进行新的真实模型认证；这些验证不证明模型永远理解正确，也不证明尚未接入的外部适配器满足条件写、权限或幂等契约。
