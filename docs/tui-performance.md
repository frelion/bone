# BONE TUI 性能验证

本文记录 TUI 的可重复性能数据集、自动资源门禁和受控本机墙钟测量。墙钟结果用于发现回退，不替代正确性测试；共享 CI 不以容易受机器负载影响的时间阈值随机阻塞提交。

## 固定数据集与门禁

性能测试位于 crate 内私有的 `crates/bone-tui/src/tests/performance.rs`，正文排版复杂度测试位于 `crates/bone-tui/src/view/components/paged_reader.rs`。

- 会话与历史：100 个会话，每个会话注入 1,000 条 `HistoryEntry`，合计 100,000 条。数据通过与 App 返回完全相同的 `UiEvent::HistoryLoaded` / `HistoryPage` 契约进入 reducer；测试不直接修改历史缓存。
- 会话切换：默认测试连续切换 2,000 次；手工 release 测试连续切换 10,000 次。
- 终端尺寸：120×40 与 160×50。
- 输入到帧：一次滚动 Action、reducer 更新和随后一帧完整渲染。
- 大正文：至少 1 MiB、超过 65,535 个视觉行的中文正文，在第 100,000 个视觉行附近深滚；连续测量复用宽度相关行索引。
- 样本数：每个墙钟场景 1,000 次。

默认测试断言以下不依赖墙钟的事实：

- 每个会话最多保留 `HISTORY_CACHE_ITEMS` 条历史；
- 100,000 条 App 形态历史进入 reducer 后，TUI 历史缓存不超过 32 MiB；
- 反复切换会话后缓存仍不超过 32 MiB；
- 1 MiB 中文正文的每帧输出只包含可见窗口，深滚复用已有行索引，不为每帧复制整份正文。

手工 release 测试同时执行硬阈值断言：

- 120×40 与 160×50 完整布局和渲染 p95 ≤ 16 ms；
- 输入 Action 到对应渲染帧 p95 ≤ 50 ms；
- 1 MiB 中文正文深滚渲染 p95 ≤ 16 ms。

运行默认资源门禁：

```sh
cargo test -p bone-tui --lib tests::performance
cargo test -p bone-tui --lib one_mib_chinese_deep_scroll
```

运行受控墙钟测量：

```sh
cargo test --release -p bone-tui --lib tests::performance::release_tui_performance_harness -- --ignored --exact --nocapture
```

应在机器空闲、无并行 Cargo 构建时运行。测试会打印操作系统、Rust、提交、工作树状态、p50、p95、最大值、历史缓存字节数，以及 Linux 上可获得的进程 RSS。首次 release 编译时间不计入样本。

## 2026-09-10 本机基线

环境：

- 机器：WSL2，Linux `6.6.87.2-microsoft-standard-WSL2`，x86_64；
- CPU：Intel Core i5-14400F，WSL 可见 16 个逻辑 CPU；
- 内存：WSL 可见 7.6 GiB；
- Rust：`rustc 1.98.0 (88d9e12ae 2026-08-18)`，LLVM 22.1.8；
- Git：`56920ca3824dc55b4dd1ec67cdebbc8edf21cedf`；测量时工作树为 dirty，因此这组数据标记为开发基线，不能冒充该提交的干净树结果。

| 场景 | p50 | p95 | max | 门禁 |
|---|---:|---:|---:|---:|
| 120×40 完整渲染 | 143.208 µs | 170.669 µs | 478.700 µs | p95 ≤ 16 ms |
| 160×50 完整渲染 | 201.609 µs | 231.450 µs | 608.909 µs | p95 ≤ 16 ms |
| 输入 Action 到 160×50 帧 | 203.405 µs | 256.953 µs | 515.301 µs | p95 ≤ 50 ms |
| 1 MiB、100,000 行附近中文正文深滚 | 219.080 µs | 354.573 µs | 2.507198 ms | p95 ≤ 16 ms |

资源结果：

- 10,000 次会话切换总耗时：8.706355 ms；
- 测量结束时 TUI 历史缓存：6,963,200 bytes，低于 32 MiB 门禁；
- 进程 RSS：测量前 2,816 KiB，测量后 19,572 KiB，增量 16,756 KiB。

RSS 增量包含 100,000 条历史的构造与处理、Ratatui 后端、1 MiB 中文源文本及其排版索引和分配器保留内存，不能等同于 TUI 历史缓存。历史缓存使用 reducer 维护的精确字节计数单独门禁；RSS 用于后续相同数据集、相同平台的趋势比较。

本次四项 p95 均通过既定阈值。最大值单独保留，用于观察调度抖动，但不作为共享 CI 的硬门禁。
