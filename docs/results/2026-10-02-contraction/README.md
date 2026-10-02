# 收缩性重构 · 2026-10-02

基线为 `main` 的 `a7ab2aa723649e79fe5c8858b60ccb8ddc38008f`，修改位于 `codex/contraction`。一个 Session/Agent、内部持久 Job、原生 Rig 接入以及事务和外部写保护边界保持不变。

## 实际删减

统计 `src/`、`tests/`、`scripts/` 中的 Rust/Python 物理行，包含空行和注释。私有测试、测试入口声明和 `cfg(test)` 函数计入测试；搬移后的文件与共享支架全部计入新总量。基线从 Git 冻结提交读取，避免把并行修改后的代码误当基线。

| 类别 | 重构前 | 重构后 | 净删除 |
| --- | ---: | ---: | ---: |
| Rust 实现 | 9,674 | 9,525 | 149 |
| Rust 测试及支架 | 8,756 | 8,135 | 621 |
| Python 测试及协议 fixture | 878 | 878 | 0 |
| Python 实验与导出脚本 | 916 | 916 | 0 |
| 合计 | **20,224** | **19,454** | **770** |

源码总量减少约 3.8%。原始逐文件计数见 [metrics.json](metrics.json)。目录搬移改善阅读顺序，不作为删减成果。

## 删除决定

- 普通 Registry 接入直接使用 `ProviderRef::completion_model()`，删除 BONE 对 SDK 环境变量解析、配置构造和 erased model 工厂的重复实现。显式凭据仍使用原生 `completion_model_with`；订阅的准备、认证与跨进程凭据锁保留各自生命周期。
- 删除没有调用方的 `connect_with`/`prepare_with` 链路。需要 HTTP 注入的显式登录仍使用原生 HTTP 类型。
- TUI 删除已完成调用集合及其淘汰策略：Engine 的 `current_call` 和 revision 已提供必要判定。合并异步任务回收、编辑键、滚动、命令帮助、原生文本投影等重复流程，删除重复宽度状态。
- HTML 导出直接投影原文中的可见字段，删除构造 synthetic JSON 再解析的中间步骤。交叉审查发现单遍处理不支持合法前向引用，恢复先索引、后关联交付；原测试现在在同一事务按 `delivery → model_message → input` 顺序验证。
- 前后历史分页共用具体读取函数；模型完成共用一次调用释放；上下文构造去除重复边界收集和派生计数。事务、当前调用检查与完整工具批次保持原语义。
- 七组集成测试共用具体 HTTP fixture、配置、临时目录、子进程回收和超时。重复设置与断言使用普通函数或数据表，场景判定保留在原契约中。
- 私有单元测试集中到 `tests/unit/`，保持模块名和故障注入能力。`tests/internal/` 保留需注入中断持久状态或检查原生模型的独立契约。阅读入口见 [测试地图](../../../tests/README.md)。
- 删除默认返回且没有断言的 `render_fixture_snapshots` 生成器。历史 SVG/TXT 留存；真实视图和 PTY 断言保留。重复的 opaque reasoning 投影断言合入既有契约，并增加交付关联和转义验证。

安全语义不同的文件路径检查，以及物理写租约、未知写核查、事务提交故障、原文与工具调用关系均保留。没有引入新的运行时框架、调度器或测试 DSL。

## 复验

- Rust：**166 项通过、1 项既有 subprocess helper 忽略**，含 3 项公开边界 compile-fail 文档检查。
- 全部目标及 feature 的 Clippy、rustfmt：通过。
- Python：6 项通过。
- Debug 真实 PTY：15/15，通过流、过期预览、新指令、暂停、问题路由、Unicode、交互编辑器 stdin 和信号退出场景。
- 优化构建安装到 `~/.cargo/bin/bone`；PATH 入口 `/opt/homebrew/bin/bone` 指向该安装，版本为 `bone 0.5.0`。

实现者交叉审查模型接入、TUI 生命周期及核心事务/上下文改动。完整命令、场景名和源码 SHA-256 见 [gates.json](gates.json)。本轮验证执行与协议契约，没有新增真实模型质量或性能实验；此前真实任务记录仍按原实验结果解释。
