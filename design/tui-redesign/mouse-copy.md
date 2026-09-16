# 鼠标指针与自动复制

状态：2026-09-16 已实施；全工作区测试与严格 Clippy 通过，release 已编译并原子替换 `/home/sunzibin/.local/bin/bone`，构建产物与安装文件 SHA-256 一致。

## 交互

- 正文、详情、标题、输入框、帮助与登录页面正文支持拖选；松开鼠标自动复制非空选区。
- 活动编辑器复用自身选区，复制后继续输入可以替换选中文字；只读内容及未激活编辑器的选区独立，不改变键盘归属。
- 普通按钮和链接在松开且仍命中原目标时激活；一旦拖选就取消点击。分栏拖拽和活动编辑器定位仍从按下开始。
- 复制范围固定于按下的内容源，不夹带相邻栏。滚动后使用原文位置维持选区；源内容替换或离开后取消失效选择。
- 输入框/正文使用文本指针，可点击控件使用手形，分栏使用左右缩放指针，空白使用默认指针。拖拽捕获期间保持相应形状。

## 内容与结构

文本选择保存来源身份、UTF-8 字节范围及原文版本。视图提供可见行到来源位置的映射，复制直接提取对应源范围，保持软折行前的连续文本、原文换行、代码缩进、Tab 与完整 Unicode 字素。过滤外部控制字符，界面装饰不作为正文。

来源定位和高亮沿用现有 FrameHits 的工作区/浮层遮挡；不从终端 Buffer 拼接文本，不新增通用控件树或事件框架。只有可见行创建字符几何。历史复制投影缓存计入预算并随替换/驱逐清理；Reader 复用原文和紧凑行起点；虚拟任务 input IDs 保持按需读取。

复制请求进入单消费者队列，按提交顺序执行，系统剪贴板调用不会阻塞键盘与鼠标处理。指针形状由当前帧和捕获派生，在 terminal 层去重输出并沿现有终端生命周期恢复。

## 平台路径

| 环境 | 复制路径 |
| --- | --- |
| 本地 macOS，包括 Terminal.app | `/usr/bin/pbcopy`，stdin 传 UTF-8，检查退出状态 |
| SSH，包括从 macOS 连接远端 | OSC 52，向客户端终端请求写入剪贴板 |
| Windows/WSL/Linux | OSC 52 |

OSC 22 用于渐进式鼠标指针增强；不支持的宿主仍使用自身指针。OSC 52 是否被接受取决于终端和 tmux 等中间层配置；发送成功并不代表宿主提供了写入回执。保留终端原生选择操作作为备用。不修改用户终端配置、不读取剪贴板。

参考：[kitty 指针协议](https://sw.kovidgoyal.net/kitty/pointer-shapes/)、[tmux 剪贴板与 macOS pbcopy](https://github.com/tmux/tmux/wiki/Clipboard)、[Windows Terminal 原生选择](https://learn.microsoft.com/en-us/windows/terminal/selection)。

## 验证

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`（包含文档测试）
- bone-tui：364 项库测试，5 项 binary 测试，12 项其他 integration 测试，12 项 PTY 测试全部通过；4 项既有库测试默认忽略。
- 新增 8 项完整鼠标复制交互契约：正反代码拖选、Unicode、链接与拖选区分、最终松开坐标、回到起点、小窗口与浮层遮挡、跨滚动详情、登录 URL、未激活输入框。
- 真实 PTY 发送鼠标协议，检查 OSC 52 负载与 UTF-8 原文一致，复制后继续输入正确替换选区，退出后的草稿持久化一致。
- 指针恢复覆盖正常退出、Unix 信号、panic、挂起与恢复。
- 显式执行两项 1 MiB Reader 既有性能测试通过；百万 input IDs 借用及复制测试通过。

验证环境是 Linux/WSL。macOS 的本地路径已实现并检查，当前环境没有真实 macOS 剪贴板或 Apple 终端，未声称完成 macOS 实机验收。

## Zed 内置终端的指针限制（2026-09-16 复核）

用户反馈指针仍为 I 型。核验运行进程与安装产物 SHA-256 一致，进程环境报告 `TERM_PROGRAM=zed`。当前 Zed 官方 `terminal_element.rs` 在普通终端内容上直接设置 `CursorStyle::IBeam`，特定链接修饰键条件才使用手形；当前绘制路径不消费应用请求的 OSC 22 形状。宿主版本或分支需要以其具体实现为准。

新增真实 PTY 回归 `hovering_without_clicks_emits_the_requested_pointer_shapes`，模拟无按键的 SGR 鼠标移动，依次断言收到 `pointer`、`ew-resize`、`text` 三种 OSC 22 请求，并验证退出恢复；测试通过。它验证 BONE 输入到输出的链路，不等于宿主已经渲染相应系统指针。此次没有修改应用行为或替换二进制。

源码依据：[Zed terminal_element.rs](https://github.com/zed-industries/zed/blob/main/crates/terminal_view/src/terminal_element.rs)。
