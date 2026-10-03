# SQLite 写锁竞争回归

日期：2026-10-03。发现来源为 Rust TUI 测试偶发失败与已安装 release 的真实 PTY 退出，属于生产并发问题。

## 原因与最小修复

侧栏异步刷新通过 `session_items` 打开独立 Store 连接。`Store::open` 的索引补写使用 `BEGIN IMMEDIATE`；引擎 `Store::commit` 原先使用 deferred 事务，先读 session snapshot，再升级到写事务。另一个连接持有写锁时，SQLite 的读→写升级可立即返回 `SQLITE_BUSY`，无法靠现有 5 秒 busy timeout 等待。

生产改动仅将 `Store::create_session` 与 `Store::commit` 的事务改为 `TransactionBehavior::Immediate`：读取前先取得写锁，竞争在入事务阶段等待。原有快照、引用、事件和原子提交验证保持不变；未优化 Store 打开或迁移路径。

## 确定性回归

测试：`store::tests::commit_waits_for_an_independent_writer_before_reading_its_snapshot`。

独立 SQLite 连接先持有 `BEGIN IMMEDIATE`，另一个线程执行真实 Store commit。busy callback 证明提交实际碰到了尚未释放的写锁；原写连接提交后，断言完整 snapshot 与原始 input 同时保存。测试依靠实际锁竞争与持久结果，不以耗时作为成功证据。

| 代码 | 结果 |
| --- | --- |
| 原 deferred 事务 | 确定性失败：`commit did not wait for the independent writer: Err("database is locked: Error code 5: database is locked")`，0.03 秒 |
| 两处 Immediate 事务 | 同一测试通过，0.01 秒 |

精确命令：

```sh
cargo test --locked --lib store::tests::commit_waits_for_an_independent_writer_before_reading_its_snapshot -- --exact --nocapture
```

## 原 TUI 失败回归

测试：`tui::tests::an_old_delivery_cannot_hide_a_new_real_model_call_or_restart_its_clock`。

修复后连续独立执行 **12 次，12 次通过**。使用既有合成凭据与 loopback fixture；未修改该测试、加入重试、禁用侧栏刷新或等待侧栏完成来掩盖竞争。

每次精确命令：

```sh
cargo test --locked --quiet --bin bone tui::tests::an_old_delivery_cannot_hide_a_new_real_model_call_or_restart_its_clock -- --exact
```

本次只构建并运行 test binary，没有构建、安装产品 binary。`git diff --check` 通过。全量 Rust、Clippy、release 重建安装和最终 PTY 由 root 统一执行。
