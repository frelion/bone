# 真实工程行动记录

离线页面：[execution-report.html](../../execution-report.html)。导出已有验收的持久 Session，不重新执行模型或工具。来源说明、里程碑与外部操作注释在 `sources.json` 中维护，注释不作为执行事件。

| 案例 | 事件 | 外部输入 | 模型启动 | Job | 结果 |
| --- | ---: | ---: | ---: | ---: | --- |
| 工程候选完整续接 | 686 | 14 | 164 | 1 | 外部纠正后功能通过并完成演示；原限时门禁仍失败 |
| 较早的多 Job 运行 | 177 | 1 | 48 | 2 | 另有 Agent 生成的内部输入；没有源码修改，额度耗尽 |

分享版本保留事件顺序、可读输入、答复、工具参数与行动；递归省略不透明 reasoning/encrypted 块及凭据/传输字段。原始 SQLite 始终只读。该版本不声称与原始事件正文逐字相同。页面和 JSON 下载均展示脱敏内容及省略计数。

来源指纹 `source_events_sha256` 对应 SQLite 原始事件数组；导出指纹 `exported_events_sha256` 对应脱敏事件数组。两者使用 sorted-key、compact UTF-8 JSON 的 SHA-256，不包含阅读投影和说明。数据库文件校验值与这两种事件指纹不同。

## 再生成

数据库不随仓库分发。manifest 中的数据库路径相对显式提供的本地验收归档根目录；只有持有对应快照时才能再生成：

```sh
python3 scripts/export_trace.py \
  --manifest docs/results/2026-10-01-execution-trace/sources.json \
  --source-root /absolute/path/to/local-acceptance-snapshots \
  --output docs/execution-report.html
```

独立导出自己的 Session：

```sh
python3 scripts/export_trace.py \
  --database /absolute/path/to/sessions.sqlite3 \
  --session SESSION_ID \
  --output /absolute/path/to/task.html
```

`--immutable` 仅适用于冻结且没有 WAL 的数据库；导出器发现 WAL 会拒绝此模式。通常使用默认只读事务。来源身份使用文件名称及 Session ID，不嵌入本机数据库绝对路径；历史对话或工具参数中的项目路径仍属于可读行动内容。

## 验证范围

历史版本通过两个 Session 的源事件、sequence、快照与阅读投影核对，26 项浏览器检查和 7 项导出检查。这些历史结果保留，不作为此次过滤修改的重新浏览器验收。

当前版本增加过滤回归：分享 JSON/HTML 不含合成的不透明推理或认证值，可读对话和工具行动保留；源事件指纹与脱敏指纹明确区分。常见凭据格式及结构字段过滤不能保证识别任意自由文本中的秘密，分享前仍须审阅内容。

HTML 可离线打开。打印只包含当时已显示的范围；“导出脱敏 JSON”包含当前 Session 全部脱敏事件。所有失败、续做和外部介入的历史说明仍保留。
