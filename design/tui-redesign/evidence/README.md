# 研究与复现记录

日期：2026-09-10。BONE `9870ed3`，本机 OpenCode `1.18.29`。研究的 OpenCode 上游版本另见 reference-version.txt。

## 实机

构建：`cargo build -p bone-tui --locked`，成功。

运行：独立 tmux server `bone-design`，BONE 参数 `--data-dir /tmp/bone-design-research/bone-data --workspace /tmp/bone-design-research/workspace`。OpenCode 在相同临时工作区，使用隔离 XDG_CONFIG_HOME 和 XDG_DATA_HOME，并禁用自动更新。没有使用真实模型发送要求。

步骤与观察：

1. 160×40 启动 BONE，记录空态：bone-empty.ansi。
2. `/new Design review`，输入第一行中文，Alt+Enter，第二行中文，↑，输入 `[UP]`，Enter。最终 `[UP]` 出现在第二行末尾；提交后截图 bone-submitted-no-model.ansi 是稳定结果。bone-editor.ansi 是输入过程中捕获，可能尚未包含最后按键，不用它证明 ↑ 行为。
3. 未选模型时，输入已显示在历史，标题和 Session 状态仍 Ready，footer 仅写 model setup needed。
4. 输入 `/`，调整至80×24，记录 bone-slash-80.ansi；调整至40×12记录 bone-40.ansi。小尺寸 footer 文本发生拼接/裁剪。
5. 120×30 创建 Second session，输入测试草稿，Ctrl+Left 与方向键浏览；记录 bone-session-switch.ansi。此轮没有做重启草稿落盘断言，不能据此声称持久化验收通过。
6. OpenCode 160×40 空态、Ctrl+P菜单、slash 各记录一份。未发送 provider 请求，真实运行中/回复样式仅以源码研究，不称作实机体验。
7. BONE Ctrl+Q 退出；OpenCode Ctrl+C 退出。临时数据仅供本次研究。

ANSI 文件来自 `tmux capture-pane -p -e`，是屏幕状态，不是全部输出字节流。boards/actual-* 由 draw.py 重建 SGR 颜色与字符位置；默认背景和16色映射是重建器选择，不代表每一种宿主终端配色。没有把这些图称为真实像素截图。

## 长回复实验

`bone-long-fixture.txt` 由 2026-09-10 的一次性外部 harness 通过 Ratatui TestBackend 生成，作为当时的历史证据保留。TUI 内部 API 收缩后，该 harness 源码已删除；当前同类场景由 crate 内私有的 `tests::preview::render_preview_artifact` 生成 SVG，使用 `BONE_TUI_PREVIEW_SCENARIO=long-reply`，尺寸和输出路径也必须显式指定。

该实验观察到 `##` 与代码围栏原样显示。它是 renderer fixture，不是 provider 流式回归，也没有据此判断工具执行能力。

## 设计自检

- draw.py 对每个文本位置执行字符宽度边界断言，对矩形执行画布边界断言。
- 使用 Browser 实际查看对话、菜单、最小尺寸，以及补充状态；检查起线、行高、选中态与提示位置。
- BOARDS.md 与 PLAN.md 是新提案，未覆盖 docs/tui.md；产品目录未修改。
- 实现后的真实终端验收、键鼠全链路、Windows/WSL和性能门禁仍需执行。
