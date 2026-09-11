# 实际 TUI 使用证据

本目录记录实际 `target/debug/bone` 的 PTY 输出。SVG/JPG 根据其 ANSI 网格重建，不是设计假数据，也不是 OS 终端截图。原始流见 `actual-session.ansi`；JSON 记录网格、尺寸和真实光标位置/隐藏状态。

## 已实际走过

- 01–06：创建会话，中文/组合字符多行输入，40 列缩放，退出并恢复普通草稿。01–05 继承 NO_COLOR，因此无彩色；06 起专用验证终端开启 truecolor。
- 08–17：32 格左栏、同宽输入/消息区，真实提交保存，设置 Session Worker，40/120/160 列布局，命令与模型菜单。
- 18–20：鼠标点击模型，无效 profile 错误保留输入，正常退出。
- 21–23：独立重命名编辑，保存返回保留中文草稿，底部命令点击。
- 24：输入与 resize 同批触发，鼠标坐标来自旧尺寸，文本实际进入 Composer；此帧不作为模型编辑成功证据。
- 25–26：分步进入模型菜单，40 列长中文模型输入及可见光标。
- 27–29：200 列消息/输入共同拉伸，正常退出。
- 30–31：首次普通输入自动创建并保存会话；发现创建中提示未清除，已修复，复验见 36。

13 是 resize 过程中的过渡帧，稳定后的 40 列布局见 14；不能把过渡帧当作最终布局。

- 32–35：无 Session 的两行中文草稿不发送直接退出，重启后完整恢复，只创建一个会话。
- 36–37：首次输入创建会话后状态提示正确，正常退出。

- 38–39：重新从真实 TUI 重试原输入，仍 Needs login，正常退出。
- 40–43：中文点击定位，插入发生在选中的grapheme边界，正常退出。
- 44–47：Shift选择替换、Ctrl+Z撤销、Ctrl+Y重做。
- 48–51：八行草稿已滚动，点击视窗顶部不跳动；鼠标拖选替换中文。
- 52–57：建立Alpha/Beta，方向键浏览保持Beta，Enter打开Alpha，滚轮保持Alpha。
- 58–60：Alt+Left定位上一词后插入，正常退出并保存。

## 外部依赖

实际 App 当前需要独立 ChatGPT 登录。真实模型回复、运行中的停止、模型发起的问题回答还没有人工跑通，不能用纯状态/渲染测试替代。没有读取或复制 Codex 凭据。

### Captures 61-79

61-62 populate actual App history with three long Chinese inputs without a model. 63-64 verify history width roundtrip. 65-68 verify mouse submit from reading and End. 69 is an accidentally selected new session, not model verification. 74 clicked the Saved description row, not an option. 76-78 repeat correctly at row26 and verify selecting the configured model despite invalid query text. 79 exits normally. JSON from63 records the launched binary SHA256. SVGs reconstruct actual ANSI cells and are not native OS terminal screenshots.

### 80-82: final login recheck

80 opens the actual command menu. 81 clicks retry for the original persisted input; the real App again reports Needs login in the session rail. No login code or credentials were requested or captured. 82 exits normally. 64-history-resize-160.jpg is a visually inspected browser capture of the faithful ANSI grid SVG, showing aligned composer/history edges and the wider rail.

### 83–86：最终二进制

最终构建SHA256前缀d57fa5609005677c；83启动，84缩到40×12，85恢复160×40，86正常退出0。完整hash在各JSON中。没有发送新请求或启动登录。

## 2026-09-11 unified /model verification

101–109 are real PTY captures of the unified model picker and connection form. The isolated App was seeded through `examples/model_entry_fixture.rs` with a non-secret API profile pointing at example.invalid; no API credential was installed. Through the real TUI, its label was changed to `API verification edited` and workspace model to `verification-model`; the helper's inspect mode read both durable facts back through App. A dummy key was typed solely to verify masking and cleared before save; it is absent from raw ANSI output. Captures104–106 exercise40×12.107 preserves the ordinary draft after returning;108 has/model and no/login command;109 exits normally.

`model-authorization-check.json` records only booleans, exit status, and binary identity. A separate real PTY with raw logging disabled entered `/model`, opened the ChatGPT form, received an actual device authorization prompt, cancelled with Esc back to Models, and exited0. No device URL/code, credential or screen dump was retained for that run. This proves entry/prompt/cancellation, not a completed account authorization or LLM response.

103-model-secret-masked.jpg is a visually inspected browser capture of the actual ANSI-grid SVG, not a native terminal screenshot.

110–116 repeat on the final compact layout: literal/model→picker→add→API form; invalid HTTP endpoint rejected visibly at40×12, masked dummy key never saved, cancel returns to picker, exit0. 112-final-api-form.jpg is the final visually inspected ANSI reconstruction. Current binary identity is in eachJSON.
