# 0.7.1：输入与运行反馈

用户实际反馈：输入看起来没有反馈、运行没有状态、默认“你 / Agent”标题过于直接。本轮修改现有 TUI，不改 Job 执行内核。

## 已确认并修正

- 状态查询先返回旧输入的交付，遮住仍在执行的后台调用。现在活动调用优先，动作与耗时取自原生启动事件；只在调用身份变化时读取和缓存。
- 刷新条件的短路求值让运行中的通知永不超时。通知过期现在每次独立检查，回执与运行状态分行。
- 编辑器只画反色格，Ratatui 隐藏了硬件光标。现在用 native TextArea 实际渲染的光标格设置终端光标，不另算换行和滚动。
- 无颜色模式清掉选区背景，现在选区还保留下划线。缩小窗口再恢复后旧视口会藏住首行，现在按输入区尺寸变化使用 native 滚动与原文光标恢复，保留选区和撤销记录。
- 默认逐条显示角色、输入短 ID 和纳入标题。现在原话用细竖线区分，普通结果直接成段；提问、失败和未交付仍显示状态。
- 弹层覆盖底部运行与输入信息。现在阅读与候选弹层限制在正文区域，底部信息持续可见。普通新要求省去目标条。

## 浏览与重现

实际终端帧及改前、改后对照见 [过程报告](overview.html)。[独立验收](acceptance-results.md)区分本地协议验证与真实订阅任务；[门禁记录](gates.json)保存构建、版本和二进制身份。

```sh
cargo test --locked
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo build --locked
python3 -B tests/tui_pty.py --case feedback-flow --binary target/debug/bone --size 80x24
python3 -B docs/results/2026-10-02-tui-design/live-software/run.py \
  --binary ~/.cargo/bin/bone --output-dir /tmp/bone-feedback-live
```

真实任务继续使用现有 Codex 登录与 `gpt-6-luna`，在隔离 Python 仓库执行测试、插话、提问、显式回复、源码修改和最终测试。原始测试文件不改，模型输出另由原测试验收；费用未知。

复杂 Markdown 阅读锚点仍为近似，模型预览变成最终持久交付时尚不保证原阅读位置连续。本轮不据单个软件任务推断通用任务成功率。
