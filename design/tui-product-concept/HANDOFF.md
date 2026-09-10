# BONE TUI 重写交接

状态：三栏对话外壳与真实 Session 闭环已实现。产品与实现的唯一有效契约见 [`docs/tui.md`](../../docs/tui.md)。本文件记录这轮重写的边界，避免后续工作重新引入已删除的旧结构。

## 已确定的产品结构

- 左栏只切换 Session，显示标题与 App 提供的短状态。
- 中栏是唯一主工作面，包含对话、实时活动和 Composer。
- 右栏是扩展面板；本轮保持空白且不可交互，后续承载 Job、材料、架构图、Git 和证据。
- 不再使用 GlobalBar、ActionBar、Dashboard、设置页面、详情 tab、创建对话框或横跨全屏的输入框。
- 用户消息使用低对比背景与细竖线，Agent 回复直接置于画布；不显示 `YOU` / `BONE` 前缀。
- 全局入口统一进入 slash command。已实现 `/new`、`/sessions`、`/rename`、`/help`、`/quit`。
- 键盘第一：`Ctrl+方向键` 空间移动焦点；鼠标点击和滚轮使用相同的 action 与 layout hit map。

## 实现结构

```text
crates/bone-tui/src
├── run/       启动、主循环、终端输入、Effect → App
├── state/     UI 状态、typed protocol、唯一 reducer
├── view/      conversation/message/composer/session_rail/slash_palette/primitives
├── layout.rs  响应式区域、命中图、transcript visual-row metrics
└── terminal.rs
```

生产 TUI 只依赖 `bone-app`，不读取 SQLite、工作区文件、Git、配置文件或 provider。为前端补齐的 App 公共能力是：幂等创建 Session、持久 last-active Session、读取 workspace resolved config、仅一次的首次输入自动标题。

## 关键正确性决定

- 创建失败或响应不确定时，`/new --retry` 必须复用原 `RequestId`。不提供 cancel，因为 Session 可能已经持久化，换新身份会重复创建。
- Submit、draft、open/release、history 的结果都绑定 Session、generation 与操作身份。
- 长消息完整保留；历史滚动以真实 wrapped visual rows 为单位。向前补读导致尾部淘汰时按同一份 metrics 补偿阅读锚点。
- 历史缓存是整个 TUI 共用 32 MiB，草稿不被淘汰。
- 外部文本控制序列失效；中文、组合字符与 emoji 编辑以 grapheme/display width 处理。
- 正常退出、Unix 信号与慢 shutdown 都先恢复 alternate screen、raw mode、鼠标和光标。

## 验证记录

- 独立正确性 reviewer 最终结论：PASS，无剩余 P0/P1。
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`：通过。
- `cargo +1.88.0 check --workspace --all-targets --all-features`：通过。
- `cargo +1.88.0 test -p bone-tui --lib --tests`：通过；45 passed，1 个手工 release 性能测试 ignored。
- `cargo test -p bone-app --all-features`：126 unit + 1 public API，通过。
- PTY 正常退出/Unix signal 终端恢复测试：通过。
- WSL release harness：120×40 render p95 约 0.36 ms；160×50 p95 约 0.46 ms；输入到帧 p95 约 0.50 ms；RSS 增量约 14 MiB。

Windows/macOS 的 CI 编译与人工终端体验仍需在对应 runner 上完成，不能以 WSL 结果替代。

## 下一轮进入点

继续按从大到小推进右栏：先定义一个通用 ExtensionPane 容器与对象选择/返回关系，再逐类接 Job、材料、架构图、Git 和证据。设置与登录仍从 slash command 进入；缺少能力时先补前端中立的 App API。不要恢复旧页面体系，也不要放入假数据或无行为按钮。
