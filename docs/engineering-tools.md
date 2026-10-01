# 工程工具

源码搜索、局部编辑和被动工具目录 CLI 由 GPT-6.1-Sol 实现，并经独立验收后集成。这与 BONE 使用 gpt-6-luna 的自主工程验收分开记分；实现过程没有修改其隔离候选或提供实现答案。

## 被动工具目录

`bone tools [--read-only] [--single-job] [--json]` 直接输出运行时实际定义的原生 Rig `ToolDefinition`。CLI 不维护第二份 schema；库的 `tool_definitions` 只提供元数据，私有工具执行器仍只能由 Engine 在 Job 内调用。

该命令在数据目录、配置、profile、模型或认证初始化之前返回。即使显式数据路径不是目录、配置损坏或 profile/model 不存在，工具发现仍可用。它不创建数据库或会话、不操作工作区、不请求模型。JSON 是原生定义数组，文本是名称与描述。

只读模式保留 `search_files`，移除 `edit_file`、`write_file`、`shell`；单 Job 模式移除跨 Job 发送、等待、交接、关闭和控制工具，保留 `read_file`、`search_files`、`ask_user` 等当前 Job 工具。

## 工程工具边界

`search_files` 接受非空字面 `query`、相对 `path`（默认 `.`）、`limit`（默认 50，范围 1～200）和可选 `cursor`。匹配按相对路径、UTF-8 字节偏移排序，行号从 1 开始；同一行每个不重叠匹配分别返回。每条结果含整个文件的 SHA-256，snippet 最多 1024 个 Unicode scalar；更长 query 的尾部可能省略。整个序列化结果最多 32768 UTF-8 字节，因此实际页可能小于请求条数。

目录遍历默认排除 `.git`、`target`、`node_modules`、`.bone`，不跟随符号链接；二进制/非 UTF-8、非普通文件及超过 16 MiB 的文件会跳过。显式 symlink 路径、绝对路径或 `..` 被拒绝。`next_cursor` 非空时继续分页；cursor 绑定查询、路径、排除项、引用文件的完整 hash 和匹配位置，允许改变页大小。引用文件缺失、重类型、内容变化或无效 token 会报错。计数只描述本次调用：`scanned_files` 为成功读取的 UTF-8 普通文件，`skipped_files` 为跳过的文件/特殊 entry；排除目录另计 `skipped_directories`，不表示整个树已搜索完毕。

cursor 是一致性位置 token，不是授权凭据或不可伪造的签名；其校验算法公开，知道算法可以构造合法位置。分页不建立整棵树的内容快照、不检测其他文件的变化，也不保证外部写入者并发变更下的跨页隔离。

`edit_file` 只处理已有 UTF-8 普通文件，携带整个原文件的 `expected_sha256` 和非空 `{old_text,new_text}` edits。所有匹配基于原文件，每个非空 `old_text` 必须唯一出现，原始跨度不得重叠；全部验证后一次安装，插入的新文本不会成为后续匹配对象。未触碰字节、CRLF 和权限保留；验证失败不安装。成功返回新 hash、字节数及 edits_applied。安装后 durability 或 cleanup 失败会保留未知结果，必须核查实际文件状态。

编辑复用已有写入准备、文件身份复查、持久化和未知结果处理。它是外部写工具，受只读权限、workspace lease、取消、过期结果所有权和未知写核查流程约束；哈希与身份复查缩小外部 writer 的竞争窗口，无法承诺原子 CAS。旧 `read_file`、`write_file`、shell 与 history 接口保持兼容。

## 验证边界

`tests/engineering_cli.rs` 检查 JSON 与实际原生定义一致、各权限组合的工具可见性、损坏/不可用配置下的发现和无会话副作用；另通过普通只读 `run`、本地 Responses fixture 和持久历史验证真实 Engine / Job 内的源码搜索、完整文件 hash、无写入和权限 schema。测试使用临时目录和合成凭据，不读取现有凭据或请求真实模型。3 项 CLI 验收与 Clippy 已通过；工程工具的函数级回归由其实现测试另行覆盖。

测试通过只能证明这些离线合同，不代表 gpt-6-luna 独立完成工程任务或外部并发写入具备原子 CAS。已有工程任务失败及之后真实候选的结果仍应分别报告。

独立冻结功能检查首次 26/27 通过，发现短行 snippet 重复附带后续行；修正后 27/27 通过。原始失败与修正后的独立 verdict 均保留。工具回归 26 项通过，CLI 回归 3 项通过；主分支统一门禁及真实模型结果在验收报告中单独记录。
