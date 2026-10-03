# 0.8.0 · 工作区交互

[浏览报告](overview.html) · [独立验收](acceptance-results.md) · [交互合同](acceptance-contract.md)

本轮公开 slash 收为 `/new`、`/model`、`/connect`、`/help`、`/quit`。左栏按会话切换；Ctrl+方向键在会话、正文与输入之间移动。Job 仍由 Agent 管理。

`/connect` 统一保存的连接、现有 Codex 登录、独立 ChatGPT 登录与原生 API 配置；`/model` 修改当前连接的原生模型名。连接与模型持久化为启动默认值，凭据不进入对话或执行事件。设置阶段不发模型探活。

## 证据范围

- `pty/`：最终安装二进制的 80×24、120×40、彩色终端过程；合成凭据和 localhost 模型协议，包含实际 Job 请求、默认值重启、草稿与焦点恢复。
- `acceptance-compatibility.json`：本轮中间构建的六个关联 PTY 回归；没有宣称全套旧场景已在最终二进制重跑。
- `live-software/`：真实 gpt-6-luna 订阅任务，复用现有 Codex 登录；运行时插话、明确回复、源码修复、12 项原测试独立复验。画面来自功能实现期间的构建，结果与构建身份见 summary。
- `pty/failure.json` 与 `sqlite-contention.md`：验收发现的真实 SQLite 并发缺陷及最小修复。失败证据保留；不把失败运行计入通过结果。
- `gates.json`：最终源、安装二进制和本地质量门结果。

最终 PTY 使用实际 alternate screen 的 VT cell、硬件光标与 SGR；HTML 是这些记录的浏览回放，ANSI 色值和字体由浏览器近似。没有使用 UI 原型代替终端证据。

## 重现

```sh
cargo test --locked
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo install --path . --locked --force
NO_COLOR=1 python3 tests/tui_pty.py --binary ~/.cargo/bin/bone --case workspace-flow --size 80x24 --evidence-dir /tmp/bone-workspace-80
```

终端本地协议检查无需真实密钥。真实订阅任务会使用模型额度，不属于离线质量门。

未扩大到所有远端 provider 的真实认证、Linux 剪贴板、全部终端主题或真实高延迟/锁定存储。独立验收保留已观察的布局限制。
