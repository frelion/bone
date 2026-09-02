# 本地 Memory 与语义搜索

[English](../memory.md) | **简体中文**

Bone 的 Memory 是完全本地、按 workspace 隔离的。JSONL 会话是唯一事实来源；LanceDB
是可重建的派生存储，位于：

```text
~/.bone/agent/memory/v1/<workspace-hash>/
```

Bone 物化的是一个对话 exchange，而不是逐条镜像 JSONL。exchange 包含 user task 与
对应的最终 assistant 回复。安全的文件路径、组件名、命令名会作为独立 reference
保存；system prompt、工具原始输出、终端日志、patch、credential 和 secret 不进入索引。

## 显式安装语义搜索

关键词搜索无需 embedding 模型。若要启用本地语义召回：

```bash
bone setup
```

它会下载并校验固定的 CPU GGUF 模型。正常 `bone` 启动绝不会下载。Bone 会在同一
进程的 Bun Worker 中通过 Bun FFI 加载 CrispEmbed/ggml 并 mmap 模型，使模型权重
不进入 Bun 的 JavaScript heap，推理也不会阻塞 TUI 主线程。

## 索引与状态

新 exchange 在持久化后同步插入 LanceDB。本地 controller 只轮询
`embeddingState = pending` 的行，按小批量 embedding，再更新同一 memory item 为
`ready`；它不会在应用层扫描 vector。LanceDB 执行 lexical/vector retrieval 并融合
候选。

运行 `/status` 可查看当前会话、Memory store、pending/ready embedding、worker
所有权、vector index 与 semantic availability。
