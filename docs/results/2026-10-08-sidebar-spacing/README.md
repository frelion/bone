# 0.9.2 会话栏设计语言

[前后对照与焦点回放](overview.html) · [质量检查](gates.json) · [构建身份](fingerprint.json)

- 名称第一行；状态和真实更新时间第二行。项间弱分隔线，两侧留白。
- 当前只名称加粗，键盘候选两行共同反色，分隔线不反色；返回主区后高亮消失。
- 状态左对齐、时间右对齐，不继承名称字重；未引入分组、图标或徽章。
- 80×24显示7项、120×40显示12项；窄屏显式展开继续复用相同规则。
- 翻页按完整可见项数；标题和信息行都打开同一会话，分隔线不改变实际会话。
- session_top仍为项目索引。固定行高共同约束布局、分页和命中；SessionItem是带实际时间的侧栏投影，不给所有Picker添加冗余字段。

100项CLI/TUI测试覆盖当前/候选的重合、分离、失焦，分隔线与信息行命中、分页、尾项、缩放与原有输入/表单边界；8项Python测试、格式、全targets/features Clippy通过。五组最终PTY数量见gates，源/脚本起止与安装文件身份一致。

```sh
cargo test --locked --quiet --bin bone
cargo fmt --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo install --path . --locked --force
NO_COLOR=1 python3 tests/tui_pty.py --binary ~/.cargo/bin/bone --case sidebar-flow --size 80x24 --evidence-dir /tmp/bone-sidebar-80
NO_COLOR=1 python3 tests/tui_pty.py --binary ~/.cargo/bin/bone --case product-flow --size 120x40 --evidence-dir /tmp/bone-product-120
```

真实运行安装版Rust TUI，问题与历史通过本地Engine/Job协议生成；32项其他标题/注意状态是机械状态板，不宣称远端模型与个人认证通过。NO_COLOR保留字重和反色。长标题仍省略，终端字体/主题尚未全面验证。

[机械空白预检包含边框的失败](attempts/gap-check-included-border/README.md)与最终通过记录分开；失败前检查把右侧竖线算入内容，尚未点击，未修改生产源。前一版完整交互记录保留原构建身份见[0.9.0](../2026-10-03-product-refactor/overview.html)。
