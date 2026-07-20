# 设置与 Provider

[English](../settings.md) | **简体中文**

使用 `/settings` 打开 Bone 的居中设置中心。修改先暂存在 overlay：`Ctrl+S` 校验并
保存且不关闭，`Esc` 或 Cancel 会丢弃草稿。

## Provider 优先配置

Bone 的唯一用户配置单位是 **Provider**。一个 Provider 包含连接信息、一份生效的
认证，以及它的模型；没有独立的 Account、API Key Profile 或会话级账号概念。

在 **Providers & Models** 中可以：

1. 选择内建预设，或 **Custom / OpenAI Compatible**。
2. 设置显示名、Base URL 和 API 协议。
3. 配置/替换 API Key，或对支持 OAuth 的 Provider 登录、登出。
4. 手动添加模型，或对兼容端点使用 **Fetch models**。
5. 只有需要时再展开 headers、compat、reasoning、thinking、token/cost 等高级字段。

Provider 定义写入 `~/.bone/agent/models.json`。secret 只写入全局
`~/.bone/agent/auth.json`，并且不会进入项目设置或 `models.json`。

## Scope 与任务模型

overlay 可在 Global / Project scope 之间切换。Project scope 遵循 trust 规则，不能
创建 credential。

`/model` 是任务模型分配菜单，目前包含：

- **Conversation**：当前聊天模型。
- **Title generation**：无参数 `/name` 使用的模型，或 **Follow Conversation**。

Plan、Design 等未来任务会复用同一路由模型。
