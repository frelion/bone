# 0.8.1 · Shift 焦点与会话左栏

[实际终端报告](overview.html) · [产品审查](product-review.md) · [独立验收](acceptance-results.md)

Shift← 进入左栏，Shift→ 返回原主焦点；Shift↑ 阅读，Shift↓ 输入。主输入区选区使用 Ctrl+Shift+方向键，表单仍使用 Shift 选区。公开 slash 命令保持五个。

左栏每项为标题、真实状态与相对时间两行。当前会话置顶并明确标识；浏览候选有两行反色，后台刷新按会话 ID 保留选择。中文长标题中间省略、保留尾部。单击标题或元信息行均打开；窄于80列时按焦点显式展开。

切换或重开保存原草稿的光标与选区；原有回复目标持久化保持。撤销历史没有跨进程持久化。只读目录查询复用一个 SQLite 连接，避免刷新列表反复运行迁移。未完成执行的暂停和未知写入核查使用既有内核边界。

## 证据

- `pty/`：最终安装版的六组 PTY。每组保留完整 VT cells、SGR、光标、事件/协议计数及 HTML 回放。
- `sidebar-flow`：机械创建32个有效闲置会话，另一次真实 Engine / Job 请求通过本地延迟协议端点；检验33项滚动、相似中文标题、焦点、点击、后台刷新和窄屏。
- `workspace-flow`：本地原生 API / ChatGPT 协议、连接和模型持久化、重启、问题回复目标及草稿恢复。使用合成凭据，不读取个人登录。
- `gates.json`：格式、235项 Rust、8项 Python、全 targets/features Clippy和安装结果。
- `fingerprints.json`：最终安装二进制与按相对 POSIX 路径排序的源码及 harness 指纹。
- `intermediate/`：六组中间构建摘要与实际问题帧，用于产品经理修正对照；不计作最终验收。
- `attempts/`：旧 harness 的“Home代表另一个会话”假设失效。当前会话置顶后，另一项应选 End；保留必要失败证据与解释。

## 重现

```sh
cargo test --locked
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo install --path . --locked --force
NO_COLOR=1 python3 tests/tui_pty.py --binary ~/.cargo/bin/bone --case sidebar-flow --size 80x24 --evidence-dir /tmp/bone-sidebar-80
NO_COLOR=1 python3 tests/tui_pty.py --binary ~/.cargo/bin/bone --case workspace-flow --size 120x40 --evidence-dir /tmp/bone-workspace-120
```

彩色回归先取消 NO_COLOR，再运行相同场景。HTML的字体和ANSI颜色由浏览器近似；不能把合成协议验收视为所有真实认证、所有Linux剪贴板和所有终端主题的证明。空态、加载失败、模态点击及状态组合由 Rust 验证，本次 PTY 未假称故障注入。
