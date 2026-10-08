# BONE 0.9.0 · 全产品打磨

本轮从2026-10-03开始，2026-10-08完成最终核对。

[实际终端报告](overview.html) · [视觉对抗](product-review.md) · [架构裁决](architecture-review.md) · [独立验收](acceptance-results.md)

左栏一项一行，当前会话与浏览候选分开，保留最高注意状态。Shift+方向键切焦点；slash候选 Enter执行、Tab补全，文件候选只插入。表单按内容收紧，末项回顾公开配置，密钥遮罩；中文连接名使用现有字段及安全路径。

所有未发送目标的草稿、光标和选区一起恢复；撤销历史不跨进程保存。上翻顶部自动读历史，失败不借旧通知计时器，点击输入恢复编辑，异步导出不抢父操作。完成的Idle会话不显示恢复提示，core暂停保护保留。

## 证据

- `pty/`：最终安装版六组真实PTY；VT cells、样式、光标、SQLite事实与协议请求计数。共120项检查/102帧。
- `product-flow`：实际Job提问，多目标草稿切换及重启，slash、点击、错误、API保存页；另一Session的12轮真实本地协议交互及历史。32个其他会话是明确标注的机械标题/状态板。
- `workspace-flow`：本地原生API/合成ChatGPT协议、连接与模型保存、重启、回复目标及草稿；不是远端OAuth。
- `baseline/`：旧0.8.1的丢稿、隐藏历史、点击输入无效；保留旧构建身份，不计入最终通过。
- `review/`：中间候选视觉对抗及中文连接实际保存，不替代最终安装验收。
- `attempts/`：保留真实宽字尾格残影及独立终端定位；另保留脚本遗漏旧待答工作及完成语义更新后旧断言的失败，均与最终通过证据分开。
- `gates.json`/logs：254项Rust、8项Python、格式、Clippy及安装结果。
- `fingerprint.json`：最终源码、二进制及harness身份。

## 重现

```sh
cargo test --locked
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo install --path . --locked --force
NO_COLOR=1 python3 tests/tui_pty.py --binary ~/.cargo/bin/bone --case product-flow --size 80x24 --evidence-dir /tmp/bone-product-80
NO_COLOR=1 python3 tests/tui_pty.py --binary ~/.cargo/bin/bone --case workspace-flow --size 120x40 --evidence-dir /tmp/bone-workspace-120
```

彩色取消NO_COLOR后跑相同场景。本轮没有个人凭据或远端模型；不宣称所有认证、终端主题与模型任务质量已经通过。取消、版本屏障和未知写核查继续由既有故障测试保护。
