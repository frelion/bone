# BONE 0.9.3：通过 /new 开始

[实际终端回放](overview.html) · [验收](gates.json) · [构建身份](fingerprint.json)

- 无输入、无工作的新会话不显示；停止事件不会让空白记录入栏。
- /new 保存并暂停真正的当前会话，直接聚焦输入；首条消息立即入栏。
- 已有 Job、待处理输入与未知写入仍可找到；不删除持久记录。
- 延续两行名称、状态、活动时间、当前与候选的区分。

102 项 CLI/TUI 与 8 项 Python 测试、fmt、全 targets/features Clippy、安装通过。两组80×24安装版PTY使用本地协议fixture，不代表远端模型或个人认证实测。源、脚本的起止身份与安装文件匹配。

```sh
cargo test --locked --quiet --bin bone
python3 -B -m unittest discover -s tests -p 'test_*.py'
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo install --path . --locked --force
python3 tests/tui_pty.py --binary ~/.cargo/bin/bone --case new-session-flow --size 80x24 --evidence-dir /tmp/bone-new-session
```
