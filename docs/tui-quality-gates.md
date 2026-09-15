# BONE TUI 质量门禁

本文定义 `bone-tui` 合入与发布前必须通过的检查。它补充 [TUI 产品与实现契约](tui.md) 和 [系统化验证策略](testing.md)，只规定可执行的验证方法、失败条件与证据，不替代产品语义。

“通过”表示本文列出的风险在指定环境和数据集下有可重复证据；它不等于宣称软件绝对没有缺陷。任何 P0/P1 正确性问题、App 边界违规、终端未恢复或性能预算失败都会阻止发布。

## 门禁分层

| 层 | 目的 | 默认执行位置 | 合入要求 |
| --- | --- | --- | --- |
| 架构边界 | 证明 TUI 只通过 `bone-app` 获得和修改产品数据 | Linux CI | 必须通过 |
| reducer 与布局 | 证明状态转换、异步归属和响应式布局 | unit / snapshot | 必须通过 |
| App 集成 | 证明生产 TUI effect 接到真实 App API 和持久层 | 临时 data dir | 必须通过 |
| PTY 与终端恢复 | 证明真实终端字节流、输入和退出行为 | Linux CI + WSL | 必须通过 |
| 故障注入 | 证明重试、竞态、中断和持久化边界 | Linux CI / 专项 helper | 必须通过 |
| 性能 | 证明常见规模与长会话不会阻塞输入或无界增长 | 固定 release 环境 | 发布必须通过 |
| 跨平台 | 证明可编译并覆盖平台特有的终端与存储行为 | Linux / macOS / Windows CI | 必须通过 |
| 人工验收 | 证明真实终端中的视觉、鼠标、中文和恢复体验 | Linux 与 WSL | 发布必须通过 |
| 对抗 review | 作者之外主动寻找反例并复验修复 | 每个实现阶段及最终提交 | 必须完成 |

## 架构边界

`bone-tui` 的生产依赖只能包含 `bone-app` 和通用前端库。以下任一情况均为阻断问题：

- manifest 直接依赖 `bone-core`、`bone-adapters`、SQLite、Git、HTTP/provider、凭据文件或项目文件访问库；
- TUI 生产代码导入 Core、Adapter 或 App 私有存储模块；
- TUI 直接读取 SQLite、工作区文件、产物、配置或凭据，或直接执行 Git / provider 请求；
- TUI 从自然语言、日志文本、文件名或 diff 猜测完成、采纳、证据、验收或未知写入状态；
- renderer、layout 或 reducer 发起 I/O、持锁等待 App，或调用系统进程；
- 测试专用 backend 能通过生产构建路径启用。

自动门禁至少包含：

```sh
cargo tree -p bone-tui --edges normal
rg -n 'bone_core|bone_adapters|rusqlite|Command::new|std::fs|tokio::fs|reqwest|credentials.toml' \
  crates/bone-tui/src crates/bone-tui/Cargo.toml
```

扫描命中不直接等同失败，例如启动 cwd 属于允许的进程边界；reviewer 必须逐条分类并记录依据。除此之外，应有一个 manifest / public-boundary 测试断言 `bone-tui` 的唯一产品依赖是 `bone-app`，并用测试 App 入口验证生产 effect 不需要访问 App 私有类型。

## Reducer 与异步状态

Reducer 测试直接向 `UiState::update` 输入事件并检查新状态和 typed effects，不启动终端或 Tokio task。测试必须覆盖：

- 页面、焦点、详情和对话框的进入/退出；`Esc` 只关闭当前层，不停止工作；
- 每个会话独立保存草稿、滚动、未读和详情选择，快速切换不会串状态；
- 提交时保存文本、草稿 revision、稳定 `RequestId` 和目标问题；迟到回执只清除完全匹配的旧版本；
- pending 期间继续输入、失败后重试和重复回执不会丢文字或产生第二个产品操作；
- 所有 App 结果携带目标身份与 generation；切换会话或详情后，迟到结果不能覆盖当前选择；
- watch 可合并，历史按 sequence 去重；空页只要 cursor 前进就保存新 cursor 并继续补读；
- 后台更新只增加未读或待处理，不抢焦点、不改变阅读锚点；
- 粘贴只产生文本编辑动作，不能映射成本地命令、确认或提交；
- 小窗口、错误、加载、取消和目标失效状态仍保留草稿及安全退出。

每个竞态测试要显式排列两个合法事件顺序，并断言相同不变量。禁止依赖 sleep、调度概率或只断言“没有 panic”。

## 布局、渲染、鼠标与 Unicode

布局测试以固定 `Rect` 验证区域，而非只比较整屏字符。必须覆盖以下边界宽高及边界前后一格：

| 尺寸 | 必须验证 |
| --- | --- |
| `160×50`、`159×50` | 三栏与中右布局切换；中央不被挤压 |
| `120×40`、`119×40` | 两栏与单主区切换；详情入口可达 |
| `80×24`、`79×24` | 主区与单区阅读切换；输入和返回可达 |
| `40×12`、`39×12`、`80×11` | 最小可用与过小提示；不触发整数下溢或越界 |

同一个 `LayoutPlan` 必须同时驱动绘制和 hit test。对每个可点击动作，测试目标中心、四条边、相邻空白和区域重叠优先级；resize 后旧坐标不得触发新页面中的动作。滚轮只滚动指针所在或当前明确聚焦的可滚动区域。

渲染测试使用 Ratatui `TestBackend`，至少覆盖空会话、长列表、详情打开、对话框、错误、提交中、窗口过小和多行 composer。断言关键文本、区域与 style 语义；只有版面整体关系稳定时才使用 snapshot，避免把无关空格变成脆弱契约。

Unicode 数据集至少包含中文、全角标点、组合字符（`e` + combining accent）、emoji、ZWJ emoji、宽字符边界、长路径、制表符和混合换行。验证：

- 截断、换行、光标和点击按显示宽度计算，不按 UTF-8 字节或 Unicode scalar 数量计算；
- 任何裁剪都保持有效 UTF-8，不拆开组合序列或 ZWJ 序列；
- 外部文字中的 CSI、OSC、C0/C1 和双向控制字符不能作为终端控制序列生效；
- 保留产品需要的换行和制表语义，过滤后显示宽度仍与光标一致。

## App 集成门禁

集成测试使用临时 data directory、真实 SQLite / journal / CAS / lease 和公开 `App` / `Session` API。TUI 通过生产 effect runner 执行动作，再把 App 的公开结果转换回 UI event；不得以直接修改 `UiState` 代替生产接线。

最少链路如下：

1. 打开 workspace，列出或创建 Session，重开后身份和标题一致；
2. 保存草稿、提交要求、接收 durable receipt，同时编辑下一条内容；重开后已保存内容和未提交草稿符合事实；
3. 从 snapshot + 分页 history hydrate，随后 observe；制造 watch 合并和过滤空页后仍无丢失、重复或停滞；
4. 回答仍有效的问题；问题失效或 Runtime 更换后拒绝旧目标，不误答新问题；
5. 保存配置后模拟 apply 失败，UI 分别展示 saved desired 与 currently applied；
6. 模拟 unknown external write、中断与重启；不自动重放，写门禁和核查依据来自 App；
7. 对同一结果版本重复提交验收只产生一个判定；退回与新的返工输入在同一 durable 产品操作中完成；
8. 读取任务、上下文、产物、证据和 workspace diff 时，所有正文、来源、分页和 Git 基线均来自 App typed API；
9. 浏览未打开会话和全局待处理队列不取得每个 Session 的 writer lease。

每条链路同时检查 UI 事实与 App 持久事实。错误注入必须落在明确的 durability boundary：确认前失败不能发布假成功；确认后响应丢失必须可用同一请求身份安全重试。

## 并发与持久化故障注入

使用 barrier、channel 或独立 helper process 控制事件顺序，不以短 sleep 推测系统进度。至少覆盖：

| 故障 / 交错 | 通过条件 |
| --- | --- |
| Session A 查询迟到，用户已切到 B | A 只更新自身缓存/未读，不改 B 的正文、选择或状态条 |
| 同一 submit 回执丢失并重试 | App 只保存一份 Input；TUI 不重复显示、不清除新草稿 |
| 保存旧草稿期间产生新 revision | 旧回执不能把新 revision 标为已保存 |
| watch lag，history 页为空但 cursor 前进 | 最终补齐公开事实，无重复且循环能够结束 |
| Question / Job / Call 属于旧 Runtime | 控制失败并显示目标失效，不作用于新 Runtime |
| 配置持久化成功、Runtime apply 失败 | desired/applied 分离，执行保持安全状态且可重试 |
| 外部效果发生后、工具结果持久化前进程退出 | 重启后标记未知写入，不自动再执行 |
| 判定已保存、返工调度响应丢失 | 相同操作重试不产生第二个判定或第二条返工输入 |
| App event channel burst / consumer lag | 控制事件不会饥饿，durable history 最终补全 |
| TUI 初始化、运行、flush 或 shutdown 失败 | 草稿尽可能保留，终端总能恢复，错误可从普通 shell 读取 |

涉及崩溃窗口的测试用独立 helper process 和临时目录，在指定 checkpoint 由父进程终止子进程，再通过新的 App 实例检查持久事实。测试不得操作开发者真实 data directory 或 workspace。

## PTY 与终端恢复

PTY 测试必须启动真实 `bone` binary，并为其提供临时 App data directory 与隔离 workspace。测试从 PTY 主端发送按键、paste、鼠标和 resize 字节，观察屏幕与子进程状态，不调用内部 reducer。

必测场景：

- 启动进入 alternate screen、raw mode、mouse capture，并在 Unix 启用 bracketed paste；正常退出前先 stop 并 join 唯一输入 worker，再依次禁用模式并显示光标；
- Unix 能力查询成功时才 push Kitty keyboard enhancement，失败时不 push，并在界面明确显示 `Shift+Enter` unavailable；
- 初始化中途失败、App open 失败、render I/O 失败、panic、`SIGINT` / `SIGHUP` / `SIGTERM` / `SIGQUIT`（平台支持处）及慢 App shutdown 后，shell 可回显、换行、光标可见且不残留鼠标协议；delegated panic hook 的输出必须晚于终端恢复；
- `SIGTSTP` 前完整恢复终端并真正停止；`SIGCONT` 后重新进入模式、重新协商能力、重建唯一输入 worker 并强制整帧绘制；
- 快速 resize 到每个布局阈值、宽高为最小值、连续鼠标滚动和点击后不 panic、不误触发；
- bracketed paste 中包含换行、快捷键字符和终端转义序列时只插入文本；
- 原生 Windows 不宣称 Crossterm 0.28 的 `Event::Paste` 能力，也不启用 bracketed-paste mode；在新的 input transport 能提供可验证 paste 边界前，该平台的含换行 paste 门禁视为未通过；
- 中文、组合字符和 emoji 的输入、退格、换行、滚动与光标位置在真实终端中一致；应用 caret 保留原 Cell 字符，并与原生 cursor 坐标一致；
- slash 建议打开以及从 Composer 切换 Session 后 caret 仍可见，退出后的最终 cursor 状态为 show；
- `Ctrl+D` 退出时先有界保存草稿，失败或超时则回到仍可用的界面；一旦收到 OS 终止信号，先恢复终端再执行有界 flush 与 App shutdown。

PTY 断言以协议序列和可观察 shell 行为为主，不能依赖某个 terminal emulator 的私有像素或配色。输出扫描拒绝 OSC、装饰性 cursor shape 和窗口 resize；测试设置总 deadline，并在失败时保存经过脱敏的 PTY transcript、终端尺寸和退出状态。

## 性能基准

基准使用 `--release`、固定数据生成器和记录过的机器/OS/Rust/提交。数据集至少包括 100 个 Session、每个 Session 的冷热状态组合、累计 100,000 条历史、长中文消息、1 MiB 工具输出和持续 App 更新。预热、采样次数及统计方法固定；报告 p50、p95、最大值与峰值 RSS。

| 指标 | 门槛 | 测量边界 |
| --- | --- | --- |
| 无网络首帧 | p95 `<=500 ms` | 进程启动至首次完整 draw |
| `120×40`、`160×50` 纯 layout + render | p95 `<=16 ms` | 已 hydrate 状态，`TestBackend` draw |
| 输入到可见帧 | p95 `<=50 ms` | 正常与持续活动更新两种负载 |
| redraw 调度 | 无固定 ticker | dirty 状态在接收下一事件前绘制；未来流式合并需单独测量 |
| 历史读取 | 首屏和上滚均分页 | 不允许为了打开 Session 读取全部历史 |
| TUI 缓存 | `<=32 MiB` | 历史与正文缓存；不含 App/Core 权威记录与草稿 |
| 长时内存 | 稳态后无持续增长 | 反复切换、打开详情和滚动 30 分钟 |
| App 操作回退 | 稳定回退 `<15%` | 与同机器、同数据的实现前基线比较 |

性能实现检查同时作为门禁：渲染路径无 I/O；正文只排版可见窗口并按内容版本/宽度缓存；每次按键不复制完整 Session view；每帧不扫描全部历史；App 查询异步执行且控制事件不会被 history/activity burst 长期阻塞。

共享 CI 不用高噪声墙钟阈值阻断合入，但必须检查缓存预算、分页调用量、dirty draw 次数和算法随数据规模的增长。固定 runner 或本地认证环境执行墙钟门槛，并把原始结果随发布候选保存。超过门槛时定位原因；不得通过扩大预算、减少数据或隐藏样本解除失败。

## Linux / WSL 人工验收

每个发布候选至少在一个 Linux 原生终端和一个 WSL 终端完成。建议覆盖 Windows Terminal + WSL，以及 Linux 上两个不同终端模拟器。验收者记录终端名称/版本、shell、locale、尺寸、提交和结果。

按以下顺序走完整产品链路：

1. 首次启动、选择 workspace、完成模型/连接设置并创建 Session；
2. 输入中文多行要求，发送期间继续编辑下一条；切换 Session 后返回，草稿和阅读位置仍正确；
3. 用鼠标和可见按钮完成会话切换、滚动、打开/关闭详情、发送、停止和返回；全程无需记快捷键；
4. 查看任务、上下文、产物、证据、workspace diff 与验收，核对来源和 HEAD 基线说明；
5. 回答待处理问题，处理配置应用失败和未知写入核查；
6. 接受一个结果，再退回另一个结果并提交返工要求；重开后判定和返工事实一致；
7. 在 160、120、80、40 列附近连续 resize，确认中央内容、返回和输入不会失联；
8. 分别走正常退出、工作中退出确认、强制中断后重开；回到 shell 后输入与光标正常。

任何不可见的关键操作、无法返回的页面、错误声明已保存/已完成、内容串 Session、中文光标错位、鼠标误触或终端污染均判定失败。人工证据保存为版本化 checklist 或 CI artifact；可以包含脱敏文本录屏，不得包含凭据、完整 prompt、授权码或私有文件正文。

## Windows 与 macOS CI

Linux CI 执行 workspace 全量 fmt、clippy、tests、doctests 和 rustdoc，并运行可自动化的 TUI reducer、layout、App integration 与 PTY 测试。

Windows 与 macOS 每次变更至少执行：

```sh
cargo check --workspace --all-targets --all-features --locked
cargo test -p bone-tui --all-targets --all-features --locked
cargo test -p bone-app --lib storage:: --locked
```

平台 job 必须验证 Rust 1.88 MSRV，或另设同等的 MSRV job；stable 通过不能替代 MSRV。平台可用 PTY 时逐步启用真实 binary 测试；暂不支持的 signal、PTY 或鼠标断言必须显式 `cfg` 并在支持平台运行，不能用全局 ignore 掩盖。

Windows 重点覆盖 ConPTY、路径前缀、CRLF、鼠标坐标、终端模式恢复和 SQLite/lease；macOS 重点覆盖终端模式、Unicode 宽度、路径、私有凭据文件装配和进程退出。平台失败是发布阻断项，除非该平台已经从公开支持范围中明确移除。

当前平台限制需要保留在发布说明中：Unix 工作区读取使用目录句柄与逐级 `NOFOLLOW`，而非 Unix 的 continuation 文件身份目前只能组合 canonical path、长度与修改时间；恶意进程若能同长度改写并精确恢复时间，自动化 CI 尚不能证明旧 cursor 必然失效。非 Unix 的 Git deadline 会终止直接子进程并停止等待，但尚无 Job Object 等价实现来保证清理恶意包装器派生的所有后代。这两项不降低 Linux/WSL 的安全语义；在 Windows 完成人工对抗验证或平台能力实现前，必须标为“自动编译与常规行为受支持，强对抗文件身份/后代清理未验证”。

## 对抗 review 与关闭条件

每个实现阶段由作者之外的两名 reviewer 独立检查最终集成代码：

- **正确性 reviewer** 主动构造竞态、数据丢失、重复执行、错误归属、持久化窗口、终端污染与敏感信息泄漏；
- **架构/性能 reviewer** 主动寻找 App 边界绕过、重复权威状态、过度抽象、无界缓存、同步阻塞、全量扫描与无效重绘。

每条 finding 必须包含触发条件和执行顺序、期望与实际结果、文件/符号或最小复现、严重度，以及修复后的验证方法。只有能复现或由代码路径直接证明的问题才作为确认缺陷；偏好性意见单独记录。

严重度与关闭规则：

| 等级 | 定义 | 关闭条件 |
| --- | --- | --- |
| P0 | 数据/凭据泄漏、重复外部写、不可恢复的数据损坏或 shell 被持续污染 | 立即阻断；回归测试、原 reviewer 复验和全套门禁通过 |
| P1 | 用户数据丢失、错误控制目标、状态虚假确认、常见路径崩溃或严重性能失控 | 阶段与发布阻断；最小回归和跨层验证通过 |
| P2 | 有明确触发条件的功能、可访问性或性能缺陷，存在可行绕过 | 默认修复后合入；延期必须有 owner、原因和跟踪项 |
| P3 | 低风险一致性或维护性问题 | 可记录后续处理，不得包装成已确认正确性风险 |

作者修复后由提出者复验。各分支分别通过不能替代对最终集成提交的再次审查。发布证据应包含：完整检查命令及提交、故障注入结果、PTY transcript 摘要、性能报告、Linux/WSL checklist、跨平台 CI 链接，以及所有 finding 的关闭状态。

## 常规执行清单

合入前至少运行：

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test -p bone-tui --all-targets --all-features --locked
cargo test -p bone-app --all-targets --all-features --locked
cargo test --workspace --all-targets --all-features --locked
cargo test --workspace --doc --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features --locked
```

发布候选在上述检查之外必须完成 PTY、故障注入、固定环境性能、Linux/WSL 人工验收和最终对抗 review。任一必需证据缺失时，状态应写为“未验证”，不能按通过处理。
