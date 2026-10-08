# 测试入口与边界

测试按证明的行为组织。共用支架只负责本地 HTTP fixture、临时目录、配置、子进程回收和超时；断言留在各场景中。

## 日常验证

```sh
cargo test --locked
cargo clippy --locked --all-targets --all-features -- -D warnings
python3 -B -m unittest discover -s tests -p 'test_*.py'
cargo build --locked
python3 -B tests/tui_pty.py --binary target/debug/bone
```

Rust 回归和 PTY 测试使用本地服务、合成凭据与临时工作区。它们验证执行契约；真实模型的任务质量另外记录在 [工程验收](../docs/verification.md)。

## 文件地图

| 位置 | 证明什么 |
| --- | --- |
| `unit/runtime.rs`、`unit/store.rs` | 调用归属、过期结果、事务故障、重启、物理写锁、中断结果不重放、分页 |
| `unit/context.rs` | 原始指令优先级、完整工具批次、按需压缩、原文引用与新结果保护 |
| `unit/model.rs`、`unit/config.rs`、`unit/state.rs` | 原生配置、凭据锁生命周期、取消与额度 |
| `unit/tools.rs`、`unit/tools_engineering.rs` | 文件竞态、哈希预期、路径边界、搜索、原子编辑及 shell 生命周期 |
| `unit/cli.rs`、`unit/tui*.rs` | CLI 默认入口、视图/Unicode、草稿、后台 Session 推进、模型后续调用、原生文本投影与 HTML 交付引用 |
| `internal/provider_contract.rs` | 原生 Rig 构造、请求/流/用量、401 单次失败 |
| `internal/runtime_safety.rs` | 注入持久化中断状态，验证恢复与输入优先级 |
| `acceptance.rs` | 通过公开 Engine/CLI 验证修复、协作、转交、打断、故障与实验统计 |
| `input_lifecycle.rs`、`resume_lifecycle.rs` | 对应输入的交付、显式结束、失败子任务与续接 |
| `question_routing.rs` | 内部 Job 提问、普通回答、chat 截止与恢复 |
| `long_session.rs`、`oversized_context.rs` | 多轮压缩、跨进程续接、长结果、共享约束与冷历史 |
| `engineering_cli.rs` | 工具元数据的被动入口、实际只读搜索、Rig 原生环境接入 |
| `generation_limits.rs` | Rig 原生受限/过滤/未知/断流终态、零提案执行、有界续做、工作与摘要机会隔离、停止/重启、append 跨进程 |
| `tui_pty.py` | 真实终端按键、流、打断、编辑器 stdin、信号退出和终端恢复 |
| `test_ablate.py`、`test_export_trace.py` | 失败/未知用量统计、外部判定与分享报告脱敏 |
| `support/`、`scripted_responses.py` | 具体的本地测试支架；不包含产品逻辑或场景判定 |

`unit/` 和 `internal/` 通过源模块的 `#[cfg(test)] #[path]` 引入，保留私有访问与原模块名。集成场景使用公开 Session 接口。`cli.rs` 避开 Cargo 对 `tests/目录/main.rs` 的独立入口自动发现。

不要用没有断言的产物生成器充当回归测试。历史视觉快照与真实运行报告保存于 `docs/results/`，按当时的代码、输入和验证范围解释。
