# 会话与 Side

[English](../sessions.md) | **简体中文**

Bone 将会话作为 workspace 的工作上下文。用户通过 Side 管理会话；底层 JSONL 文件
属于实现细节。

## 焦点与导航

- `Shift+Left` / `Shift+Right`：在对话和 Side 之间移动焦点。
- Side 获得焦点后：`↑` / `↓` 选择会话，`Enter` 打开。
- 即使 Side 有焦点，`Ctrl+C` / `Ctrl+D` 仍维持原有的清空/退出语义。

切到另一会话不会中断正在运行的会话。活跃会话继续在后台运行；任务 settled 后，
它的 renderer/runtime 会释放，避免隐藏会话不断累积常驻 runtime。

## 搜索与删除

使用 Side 搜索来查找当前 workspace 的会话。关键词结果会立即出现；安装本地
embedding 模型后，语义结果会异步补充。

Side 获得焦点时按 `d` 请求删除，`Enter` 确认，`Esc` 取消。删除是软删除：Bone
优先移动到系统废纸篓，失败时放进自己的 session trash。删除当前前台会话前，会先
切到相邻会话；若没有其他会话，则先创建空会话。

## 会话名称

`/name <文本>` 手动命名。无参数 `/name` 使用配置的 title-generation 任务模型生成
简洁名称，且不会向对话写入新消息。`/model` 分别配置 Conversation 和 Title
generation 模型。
