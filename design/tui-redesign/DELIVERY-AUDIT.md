# BONE TUI 交付核对（进行中）

按用户完整交付目标和 B 方向设计/交互说明核对，而非用当前代码反向定义完成。后续明确反馈优先：左栏 32 格；输入与消息面同宽；正文上下留白；纯黑与橘黄；简单 App-only 架构。

## 证据与缺口

| 要求 | 当前证据 | 结论 |
|---|---|---|
| 三栏、配色、细选择线、同宽输入、上下留白 | layout/view 生产代码；PTY 08/12/14/22/28；断点与几何测试 | 已实现并在 macOS 实际使用 |
| Unicode 输入、视觉行与实心光标 | text.rs 测试；PTY 中文多行/重启；原始 ANSI 含 SteadyBlock；40–42 鼠标定位插入 | 已验证上述范围；IME/终端字体仍需原生人工体验 |
| 文本选择、拖选、撤销重做、按词移动 | 原 INTERACTION §2 明确要求；前版 input.rs 缺少动作 | 已实现并完成实际 PTY 44–51/58–59 选择替换、撤销重做、拖选、按词插入与稳定视窗检查 |
| 会话先浏览候选再 Enter 打开、滚轮不切会话 | 原 INTERACTION §5；当前 SelectPrevious/Next 直接选会话 | 已实现；PTY 55–57 验证浏览不换会话、Enter打开及滚轮保持当前会话 |
| 首次输入、普通草稿、回答草稿安全退出 | state/runtime 契约测试；PTY 30–37；失败复用创建身份 | 已实现；真实无会话草稿退出重启通过 |
| 模型设置与实际配置列表、错误保留输入 | models public App 集成；PTY 10/18/19/25/26 | 已验证；实际模型执行仍待登录 |
| 登录取消与旧事件隔离 | lifecycle/identity 回归；38 实际重试仍 Needs login | 实际登录成功未验证 |
| 正常回答、失效拒答、显式转草稿 | answer_contract 与 reducer；完整 QuestionId | 代码/状态测试已覆盖；真实模型问题流程未验证 |
| 详情完整内容与返回 | reader 单元测试；panel 草稿/焦点回归 | Job/工具已接；真实模型产物的实际操作未验证 |
| 已打开 Job 随当前事实刷新 | 本轮修复 refresh_job；有效 generation、完整身份和消失回归 | 已修复；历史详情保持不可变 |
| 详情来源高亮、键盘访问多个对象、resize阅读锚点 | 源码及原 INTERACTION §6/9 | 需要进一步核查/补齐，现有 latest 快捷入口不能证明全部对象可达 |
| 重试不重复输入、停止范围、恢复原文 | answer/reducer/runtime；38 实际 retry 后 Needs login | 状态与接线证据具备；真实运行停止/恢复尚未人工验收 |
| 空右栏无操作、菜单无穿透、重命名保草稿 | hit/layout/input 测试；PTY 21–23 | 已实现并验证对应范围 |
| App-only、简单结构 | dependency boundary；生产依赖只有 bone-app 和前端库 | 通过现有边界检查 |
| 全部质量/发布门禁 | tui-quality-gates.md；本机 tests/clippy/release 输出 | 不等于全部发布门禁通过，见下方 |

## 质量门禁范围

此前已通过的 96 项 TUI 测试、workspace Clippy、fmt 和 release 性能是当时集成版本证据；本轮增加点击定位及 Job 刷新后重跑对应测试。最终检查以新日志为准。原性能报告缺 RSS、30 分钟稳态、完整端到端输入负载证据，不把短 harness 当作完整长时门禁。

本机为 macOS。Linux/WSL 人工验收、Windows/macOS/Linux 最终 CI、MSRV 和故障注入矩阵必须分别举证。CI 文件存在不是运行通过证据。尚未发布或合并，不能称达到发布条件。

## 当前外部条件

在隔离验证 workspace 通过实际 TUI 重试后，App 仍返回 Needs login（38）。需要用户完成 BONE 独立 ChatGPT 登录，不能复制 Codex 凭据或用静态假数据冒充真实执行。这个条件不阻止继续补齐上表仍缺失的编辑、浏览与阅读行为，所以 goal 保持进行中，不标完成或 blocked。


## 本次新增证据

- `cargo test --workspace --all-targets --all-features --locked` 本机完整运行通过（`/tmp/bone-audit-workspace-tests.log`），包含本次点击定位与 Job 刷新版本；后续编辑器/会话浏览更改仍需再验相关目标。
- 40–43 真实 PTY：点击两行中文第一行指定位置，屏幕文字不变、光标落在 [44,34]，随后插入落在「没有【定位】发送…」正确边界，正常退出。
- CI 的 macOS/Windows job 从仅 lib 改为 TUI all-targets/all-features，与已有质量门禁一致；未推送、未声称远端 CI 通过。


后续补验：Rust 1.88.0 已实际安装并运行 `cargo +1.88.0 check --workspace --all-targets --all-features --locked --target-dir /tmp/bone-msrv-target` 通过；这是 macOS MSRV 编译证据，不能替代 Linux/Windows 运行。TUI 编辑器/会话浏览集成后的包测试 112 项通过，2 项手动性能单独运行。

Reader 性能修复已实测：1MiB普通文本连续滚动平均0.206ms（修复前43.88ms），纯换行0.120ms；初次排版仍38–49ms，不能将热缓存数字用于冷排版。准入8MiB包括原投影和packed行数据，实测约2.05MiB/5MiB。特殊超预算对象保全文但不缓存，冷排版、进程RSS及长时门禁仍需按范围说明。

## Final integration verification

- TUI all-targets/all-features: 128 passed, 3 ignored; manually invoked performance evidence is separate. Log: /tmp/bone-final-tests.log.
- Workspace Clippy with warnings denied, Rust 1.88 all-targets/all-features check, workspace doctests and diff check passed. Logs: /tmp/bone-final-clippy.log, /tmp/bone-final-msrv.log, /tmp/bone-final-doc.log.
- Objects menu and original-byte history anchors are now implemented, superseding the earlier incomplete-object-navigation row. Real model-generated objects remain unverified.
- Actual PTY 63-64: Composer PageUp, width 160/40/120/80/160; final screen exactly equals the starting screen.
- Actual PTY 65-68: clicking submit from reading focus saves and clears the submitted draft; End returns to latest.
- Actual PTY 76-78: entering an invalid model query then clicking a configured model selects that model and closes the panel.
- Late model replies cannot disturb a newer panel; background submit errors cannot replace another session status. Regression coverage added.
- Packed history anchors count toward the original history budget. Release 160x50 render p95 1.011ms; input-to-render p95 1.053ms. These do not establish process RSS or long-duration gates.
- Actual model execution still requires BONE login. Goal remains incomplete.

## Subsequent final-worktree audit

Workspace all-targets/all-features tests, workspace all-features doctests, and warnings-denied rustdoc passed in /tmp/bone-delivery-{workspace,doctests,rustdoc}.log. Independent correctness review found no new confirmed P1/P2 in the four repaired interactions and history-anchor integration; this was code review, not live model testing. Independent performance review identified an oversize transcript-metrics eviction defect; correction is in progress. The previously documented oversized Reader source admission limitation remains open and must not be represented as a hard total-memory guarantee. Actual PTY81 confirms the login prerequisite persists.

### Budget review correction

The metrics admission/eviction defect is repaired, including the LayoutPlan Arc reference. Structured tool JSON now uses compact lossless output; a <=1MiB nested outcome that formerly pretty-printed above8MiB passes a lossless roundtrip and escape-safety regression. Latest TUI checks:131 passed,3 ignored; Clippy passed (/tmp/bone-budget-tests.log,/tmp/bone-budget-clippy.log).

A separate very-long-lived Job boundary remains: cumulative input IDs have no count ceiling, and joining every ID into the Reader creates an unbounded duplicate. A bounded related-input page is being designed; all IDs must remain reachable without increasing the budget. This is open implementation work, so the goal cannot yet be considered blocked solely by login or complete.

## 本轮收尾结果

- 极长 Job 的关联输入已改为按当前 viewport 借用 App snapshot 中的 ID 切片，完整 session/runtime/job 身份校验；没有整表复制或逐 ID 缓存。百万 ID、末尾 u64、窄屏、刷新与失效测试通过，独立 reviewer 复验通过。上文这一 open 项已关闭。
- 工具错误显示标题及最多三行摘要；最多扫描4096字节安全前缀，完整详情保留，Unicode及控制字符回归通过。
- 最新包级全目标测试136通过、3忽略；workspace Clippy、fmt与build通过。日志 /tmp/bone-complete-{tests,clippy,build}.log。
- 最终二进制实际 PTY83–86启动、40×12最小布局、恢复160×40及退出0；JSON包含完整SHA256，前缀d57fa5609005677c。
- 已完成本轮确认缺陷的实现及独立复验。完整模型问答、实际工具详情、运行中停止/恢复仍未证明：实际 App 重试继续 Needs login。不得将当前状态称为完整交付成功。
- 发布范围证据仍按上文标未验证：Linux/WSL原生人工验收、远端平台CI、完整30分钟RSS与正式发布门禁。未提交、未发布。

## 2026-09-11 恢复检查（第1次）

恢复后实际启动最终二进制并通过命令菜单重试原持久输入；PTY87再次显示 Needs login，PTY88正常退出0。未读取或复制凭据，未重新提交请求。上一轮是实现及验证进展，本次只确认登录前提仍缺失，尚不能验证真实模型链路。按恢复后的新计数保留goal active，不提前重新标blocked。

## 2026-09-11 用户纠正后的接入入口交付

此前把ChatGPT登录当作统一前提的判断不成立。现在/model是唯一模型相关slash入口，API配置、模型选择和账号授权在同一面板完成；旧/login及旧原始profile文本编辑状态已移除。App新增按预期Profile保存Key的接口，把凭据槽绑定捕获的endpoint，避免并发改地址写错目标。

实际PTY101–116已验证API表单、遮罩、保存配置并从App读回、40×12、校验失败、返回与普通草稿保留；单独无日志PTY验证真实设备授权提示及取消返回，授权码未保留。最新TUI149项通过、3项性能手动项忽略；工作区all-targets/all-features测试、workspace Clippy、Rust1.88检查和构建通过（/tmp/bone-model-workspace-*.log、/tmp/bone-model-msrv.log）。三条独立复审finding均已复验关闭。

这证明统一接入功能和上述操作可用，不表示已经用真实API Key完成远端模型请求或完成账号授权；完整模型产品链路仍需任一有效接入，不能再指定只有ChatGPT登录一种。
